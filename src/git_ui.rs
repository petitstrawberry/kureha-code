//! Git in the workbench: the branch and change count in the status bar,
//! change marks in the explorer, the Git sidebar (staged and unstaged changes,
//! the commit message) and diff tabs. Repositories are read on the Git worker;
//! stages and commits run in order on the Git writer.
use super::*;
use kureha_code::{
    explorer::Row,
    file_icons,
    git::{
        Change, Entry, Outcome, Snapshot, Watcher,
        diff::{self, FileDiff, Side},
        write::{Operation, Writer},
    },
};
use scarlet_ui::{prelude::State as Ui, views::Truncation};
use std::{
    result::Result,
    time::{Duration, Instant},
};

/// How often the folder is read again, for changes made outside the editor.
const REFRESH_INTERVAL: Duration = Duration::from_secs(3);
/// Slow reads (many files whose times changed but whose contents did not, as
/// the index is never refreshed) wait this many times their duration.
const SLOW_READ_FACTOR: u32 = 10;

/// Git state kept in the app runtime.
#[derive(Default)]
pub(super) struct State {
    watcher: Option<Watcher>,
    started: bool,
    unavailable: Option<String>,
    /// Bumped whenever the folder changes; older answers are ignored.
    generation: u64,
    folder: Option<PathBuf>,
    requested: Option<Instant>,
    waiting: bool,
    /// How long the last answered read took.
    last_read: Duration,
    snapshot: Option<Arc<Snapshot>>,
    writer: Option<Writer>,
    writes: u64,
    /// Operations sent to the writer and not answered yet.
    pending: usize,
    pub(super) diffs: BTreeMap<TabId, DiffTab>,
    diff_answers: Option<(mpsc::Sender<DiffAnswer>, mpsc::Receiver<DiffAnswer>)>,
    diff_requests: u64,
    /// Status bar progress items of operations sent to the writer, in order.
    progress: std::collections::VecDeque<u64>,
}

/// A tab showing one change.
pub(super) struct DiffTab {
    pub(super) workdir: PathBuf,
    /// Relative to the work tree.
    pub(super) path: PathBuf,
    pub(super) side: Side,
    /// The newest request; older answers are dropped.
    request: u64,
    pub(super) diff: Option<Result<Arc<FileDiff>, String>>,
}
type DiffAnswer = (TabId, u64, Result<FileDiff, String>);

/// The Git sidebar's own state.
#[derive(Clone)]
pub(super) struct Panel {
    owns_focus: Ui<bool>,
    focus_message: Ui<bool>,
    rows: Ui<Vec<GitRow>>,
    selected: Ui<Option<usize>>,
    message: Ui<String>,
    message_selection: Ui<scarlet_ui::views::TextSelection>,
    /// An operation failure or the reason Git is unavailable.
    notice: Ui<String>,
    busy: Ui<bool>,
    discard: Ui<Option<(PathBuf, Entry)>>,
}
impl Panel {
    pub(super) fn new() -> Self {
        Self {
            owns_focus: Ui::new(generate_state_id(), false),
            focus_message: Ui::new(generate_state_id(), false),
            rows: Ui::new(generate_state_id(), Vec::new()),
            selected: Ui::new(generate_state_id(), None),
            message: Ui::new(generate_state_id(), String::new()),
            message_selection: Ui::new(generate_state_id(), Default::default()),
            notice: Ui::new(generate_state_id(), String::new()),
            busy: Ui::new(generate_state_id(), false),
            discard: Ui::new(generate_state_id(), None),
        }
    }
    pub(super) fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![
            &self.owns_focus,
            &self.focus_message,
            &self.rows,
            &self.notice,
            &self.busy,
            &self.discard,
        ]
    }
    /// The sidebar was clicked last, so the editor must not take focus.
    pub(super) fn owns_focus(&self) -> bool {
        self.owns_focus.get()
    }
    pub(super) fn blur(&self) {
        self.owns_focus.set(false);
        self.focus_message.set(false);
    }
}

/// A row of the sidebar's change list.
#[derive(Clone, Debug, PartialEq)]
pub(super) enum GitRow {
    Header { staged: bool, count: usize },
    File { entry: Entry, staged: bool },
}
impl GitRow {
    fn key(&self) -> String {
        match self {
            Self::Header { staged, .. } => format!("header:{staged}"),
            Self::File { entry, staged } => format!("{staged}:{}", entry.path.display()),
        }
    }
}

/// Staged changes first, then the rest, each under a header.
fn rows_for(snapshot: &Snapshot) -> Vec<GitRow> {
    let mut rows = Vec::new();
    for (staged, entries) in [(true, &snapshot.staged), (false, &snapshot.unstaged)] {
        if entries.is_empty() {
            continue;
        }
        rows.push(GitRow::Header {
            staged,
            count: entries.len(),
        });
        rows.extend(entries.iter().map(|entry| GitRow::File {
            entry: entry.clone(),
            staged,
        }));
    }
    rows
}

/// The file a diff tab shows and its label: `lib.rs · Staged`.
pub(super) fn diff_tab_label(diff: &DiffTab) -> (PathBuf, String) {
    let name = diff.path.file_name().map_or_else(
        || diff.path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let side = match diff.side {
        Side::Staged => "Staged",
        Side::Unstaged => "Changes",
    };
    (diff.workdir.join(&diff.path), format!("{name} · {side}"))
}

fn change_color(change: Change, colors: &kureha_code::theme::WorkbenchColors) -> Color {
    match change {
        Change::Conflicted => colors.error,
        Change::Modified | Change::Deleted => colors.warning,
        Change::Untracked | Change::Added | Change::Renamed => colors.accent,
    }
}

/// What the status bar says for an answer.
fn status_text(outcome: &Outcome) -> String {
    match outcome {
        Outcome::Ready(snapshot) => snapshot.summary(),
        Outcome::NotRepository | Outcome::Unavailable(_) => String::new(),
        Outcome::Failed(error) => format!("Git: {}", error.lines().next().unwrap_or(error)),
    }
}

impl App {
    /// The open folder, else the active file's folder.
    fn git_folder(&self) -> Option<PathBuf> {
        let active = self.active_id();
        let rt = self.runtime.lock().unwrap();
        rt.explorer.root.clone().or_else(|| {
            rt.workspace
                .get(active?)?
                .path
                .as_ref()?
                .parent()
                .map(PathBuf::from)
        })
    }
    /// Reads the repository again at the next poll (after a save, say).
    pub(super) fn refresh_git(&self) {
        self.runtime.lock().unwrap().git.requested = None;
    }
    /// Sets each explorer row's change from the latest snapshot.
    pub(super) fn apply_git_marks(&self, rows: &mut [Row]) {
        let snapshot = self.runtime.lock().unwrap().git.snapshot.clone();
        for row in rows {
            row.git = match (&snapshot, &row.path) {
                (Some(snapshot), Some(path)) if row.directory => snapshot.folder_change(path),
                (Some(snapshot), Some(path)) => snapshot.change(path),
                _ => None,
            };
        }
    }
    pub(super) fn poll_git(&self) {
        let folder = self.git_folder();
        let (answer, cleared, failed_to_start) = {
            let mut rt = self.runtime.lock().unwrap();
            let state = &mut rt.git;
            if state.unavailable.is_some() { return; }
            let mut failed_to_start = None;
            if !state.started {
                state.started = true;
                match Watcher::start() {
                    Ok(watcher) => state.watcher = Some(watcher),
                    Err(e) => failed_to_start = Some(e),
                }
            }
            let Some(watcher) = &state.watcher else {
                drop(rt);
                if let Some(error) = failed_to_start {
                    self.status.set(error);
                }
                return;
            };
            let mut answer = None;
            if let Some((generation, answered, outcome)) = watcher.poll()
                && generation == state.generation
                && state.folder.as_ref() == Some(&answered)
            {
                state.waiting = false;
                state.last_read = state.requested.map_or(Duration::ZERO, |at| at.elapsed());
                answer = Some(outcome);
            }
            let cleared = folder != state.folder;
            if cleared {
                state.folder = folder;
                state.generation += 1;
                state.requested = None;
                state.waiting = false;
                state.snapshot = None;
                answer = None;
            }
            if let Some(folder) = &state.folder {
                let interval = REFRESH_INTERVAL.max(state.last_read * SLOW_READ_FACTOR);
                let due = state.requested.is_none_or(|at| at.elapsed() >= interval);
                if due && !state.waiting {
                    watcher.request(state.generation, folder.clone());
                    state.requested = Some(Instant::now());
                    state.waiting = true;
                }
            }
            (answer, cleared, failed_to_start)
        };
        if let Some(error) = failed_to_start {
            self.status.set(error);
        }
        let mut marks_changed = cleared;
        if cleared { self.git_panel.notice.set(String::new()); }
        if cleared && !self.git_status.get().is_empty() {
            self.git_status.set(String::new());
        }
        if let Some(outcome) = answer {
            let text = status_text(&outcome);
            if self.git_status.get() != text {
                self.git_status.set(text);
            }
            let snapshot = match outcome {
                Outcome::Ready(snapshot) => Some(snapshot),
                Outcome::Unavailable(reason) => {
                    self.git_panel.notice.set(reason.clone());
                    let mut rt = self.runtime.lock().unwrap();
                    rt.git.unavailable = Some(reason);
                    rt.git.watcher = None;
                    marks_changed = true;
                    None
                }
                _ => None,
            };
            let mut rt = self.runtime.lock().unwrap();
            if rt.git.snapshot.as_deref() != snapshot.as_ref() {
                rt.git.snapshot = snapshot.map(Arc::new);
                marks_changed = true;
            }
        }
        if marks_changed {
            self.refresh_tree();
            let snapshot = self.runtime.lock().unwrap().git.snapshot.clone();
            let rows = snapshot.as_deref().map(rows_for).unwrap_or_default();
            if self.git_panel.rows.get() != rows {
                self.git_panel.selected.set(None);
                self.git_panel.rows.set(rows);
            }
            self.request_diffs(None);
        }
        self.poll_git_writer();
        self.poll_diffs();
    }

    /// Shows the Git sidebar.
    pub(super) fn show_git(&self) {
        self.show_sidebar(super::sidebar_ui::Sidebar::Git);
        self.refresh_git();
    }
    /// The Git sidebar is shown and holds keyboard focus.
    pub(super) fn git_wants_focus(&self) -> bool {
        self.sidebar.get() == super::sidebar_ui::Sidebar::Git
            && !self.project.is_open()
            && self.git_panel.owns_focus()
    }

    fn git_write(&self, operation: Operation) {
        let started = {
            let mut rt = self.runtime.lock().unwrap();
            let Some(workdir) = rt.git.snapshot.as_ref().map(|s| s.workdir.clone()) else {
                return;
            };
            let state = &mut rt.git;
            let mut started = Ok(());
            if state.writer.is_none() {
                match Writer::start() {
                    Ok(writer) => state.writer = Some(writer),
                    Err(e) => started = Err(e),
                }
            }
            if let Some(writer) = &state.writer {
                state.writes += 1;
                state.pending += 1;
                writer.submit(state.writes, workdir, operation.clone());
            }
            started
        };
        match started {
            Ok(()) => {
                let doing = match &operation {
                    Operation::Stage(_) => "Staging…".to_string(),
                    Operation::Unstage(_) => "Unstaging…".to_string(),
                    Operation::Discard(_) => "Discarding changes…".to_string(),
                    Operation::Commit(_) => "Committing…".to_string(),
                    Operation::CreateBranch { name, .. } => format!("Creating {name}…"),
                    Operation::Switch { name, .. } => format!("Switching to {name}…"),
                };
                self.git_panel.busy.set(true);
                self.git_panel.notice.set(String::new());
                let id = self.progress(doing);
                self.runtime.lock().unwrap().git.progress.push_back(id);
            }
            Err(e) => {
                self.git_panel.notice.set(e.clone());
                self.notify(super::messages::Level::Error, e);
            }
        }
    }
    fn poll_git_writer(&self) {
        let answers = {
            let mut rt = self.runtime.lock().unwrap();
            let state = &mut rt.git;
            let Some(writer) = &state.writer else {
                return;
            };
            let answers = writer.poll();
            state.pending -= answers.len().min(state.pending);
            if !answers.is_empty() {
                // Read the repository again now rather than at the next tick.
                state.requested = None;
            }
            (answers, state.pending)
        };
        let (answers, pending) = answers;
        for (_, operation, result) in answers {
            // Answers come in the order the work was sent.
            let progress = self.runtime.lock().unwrap().git.progress.pop_front();
            if let Some(id) = progress {
                self.progress_done(id);
            }
            match result {
                Ok(done) => {
                    if matches!(operation, Operation::Commit(_)) {
                        self.git_panel.message.set(String::new());
                    }
                    if matches!(operation, Operation::Discard(_) | Operation::Switch { .. } | Operation::CreateBranch { switch: true, .. }) {
                        self.reload_switched(&done.changed);
                    }
                    self.git_panel.notice.set(String::new());
                }
                Err(e) => {
                    self.notify(super::messages::Level::Error, e.clone());
                    self.git_panel.notice.set(e);
                }
            }
        }
        if pending == 0 && self.git_panel.busy.get() {
            self.git_panel.busy.set(false);
        }
    }
    pub(super) fn git_stage(&self, entries: Vec<Entry>, staged: bool) {
        if staged {
            let mut paths = Vec::new();
            for entry in entries {
                paths.extend(entry.source);
                paths.push(entry.path);
            }
            self.git_write(Operation::Unstage(paths));
        } else {
            self.git_write(Operation::Stage(
                entries.into_iter().map(|e| e.path).collect(),
            ));
        }
    }
    fn request_git_discard(&self, entry: Entry) {
        let rt = self.runtime.lock().unwrap();
        if rt.git.pending != 0 || entry.folder || !matches!(entry.change, Change::Modified | Change::Deleted) { return; }
        let Some(snapshot) = &rt.git.snapshot else { return; };
        self.git_panel.discard.set(Some((snapshot.workdir.clone(), entry)));
    }
    fn confirm_git_discard(&self) {
        let Some((workdir, entry)) = self.git_panel.discard.get() else { return; };
        self.git_panel.discard.set(None);
        let rt = self.runtime.lock().unwrap();
        if rt.git.pending != 0 || rt.git.snapshot.as_ref().is_none_or(|s| s.workdir != workdir) { return; }
        let target = kureha_code::git::branch::resolved(&workdir.join(&entry.path));
        if rt.workspace.documents.iter().any(|doc| doc.editor.dirty() && doc.path.as_ref().is_some_and(|p| kureha_code::git::branch::resolved(p) == target)) {
            drop(rt);
            self.git_panel.notice.set("Save or undo unsaved editor changes before discarding this file.".into());
            return;
        }
        drop(rt);
        self.git_write(Operation::Discard(vec![entry.path]));
    }
    pub(super) fn git_can_operate(&self) -> bool {
        self.runtime.lock().unwrap().git.snapshot.is_some()
    }
    /// Local branches of the open repository, from the latest read.
    pub(super) fn git_branches(&self) -> Vec<kureha_code::git::branch::Branch> {
        let rt = self.runtime.lock().unwrap();
        rt.git
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.branches.clone())
            .unwrap_or_default()
    }
    /// Files open with unsaved edits, which a switch must not change.
    fn unsaved_files(&self) -> Vec<PathBuf> {
        let rt = self.runtime.lock().unwrap();
        rt.workspace
            .documents
            .iter()
            .filter(|doc| doc.editor.dirty())
            .filter_map(|doc| doc.path.clone())
            .collect()
    }
    pub(super) fn switch_branch(&self, name: String) {
        if self.git_branches().iter().any(|b| b.current && b.name == name) {
            return;
        }
        let keep = self.unsaved_files();
        self.git_write(Operation::Switch { name, keep });
    }
    pub(super) fn create_branch(&self, name: String) {
        self.git_write(Operation::CreateBranch { name, switch: true });
    }
    /// After a switch: open documents follow their files (the switch refused
    /// to touch any with unsaved edits); those whose file is gone close.
    fn reload_switched(&self, changed: &[PathBuf]) {
        let Some(workdir) = self
            .runtime
            .lock()
            .unwrap()
            .git
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.workdir.clone())
        else {
            return;
        };
        use kureha_code::git::branch::resolved;
        // Open documents may spell the folder differently (`/tmp` and
        // `/private/tmp`); compare resolved paths.
        let open: Vec<(PathBuf, kureha_code::workspace::DocumentId)> = {
            let rt = self.runtime.lock().unwrap();
            rt.workspace
                .documents
                .iter()
                .filter_map(|doc| Some((resolved(doc.path.as_ref()?), doc.id)))
                .collect()
        };
        for relative in changed {
            let path = workdir.join(relative);
            let target = resolved(&path);
            let Some(id) = open
                .iter()
                .find_map(|(open, id)| (*open == target).then_some(*id))
            else {
                continue;
            };
            match kureha_code::files::read(&path) {
                Ok(raw) => {
                    {
                        let mut rt = self.runtime.lock().unwrap();
                        let Some(doc) = rt.workspace.get_mut(id) else {
                            continue;
                        };
                        if doc.editor.dirty() || doc.image.is_some() {
                            continue;
                        }
                        let text = raw.replace("\r\n", "\n").replace('\r', "\n");
                        let len = doc.editor.document.len();
                        // Keep the caret near where it was, on a character boundary.
                        let mut end = text.len().min(doc.editor.selection.caret.byte);
                        while !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        doc.editor.edit_parts(
                            &[scarlet_ui::views::EditDelta {
                                replaced_range: 0..len,
                                inserted: text,
                            }],
                            scarlet_ui::views::TextSelection::collapsed(end),
                        );
                        doc.editor.saved();
                        doc.crlf = raw.contains("\r\n");
                        doc.disk_text = Some(raw.into());
                    }
                    self.sync_editor(id);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    let clean = self
                        .runtime
                        .lock()
                        .unwrap()
                        .workspace
                        .get(id)
                        .is_some_and(|doc| !doc.editor.dirty());
                    if clean {
                        self.remove_document(id);
                    }
                }
                Err(_) => {}
            }
        }
        self.changed();
    }
    fn git_commit(&self) {
        if self.git_panel.busy.get() {
            return;
        }
        let message = self.git_panel.message.get();
        self.git_write(Operation::Commit(message));
    }

    pub(super) fn is_git_diff_tab(&self, id: TabId) -> bool {
        self.runtime.lock().unwrap().git.diffs.contains_key(&id)
    }
    /// Opens (or shows) the tab for one change.
    fn open_git_diff(&self, entry: &Entry, staged: bool) {
        if entry.folder || self.blocked() {
            return;
        }
        let side = if staged { Side::Staged } else { Side::Unstaged };
        let (tab, existing) = {
            let mut rt = self.runtime.lock().unwrap();
            let Some(workdir) = rt.git.snapshot.as_ref().map(|s| s.workdir.clone()) else {
                return;
            };
            let found = rt.git.diffs.iter().find_map(|(tab, diff)| {
                (diff.workdir == workdir && diff.path == entry.path && diff.side == side)
                    .then_some(*tab)
            });
            match found {
                Some(tab) => (
                    tab,
                    rt.group_tabs.iter().position(|tabs| tabs.contains(&tab)),
                ),
                None => {
                    let tab = rt.workspace.new_tab_id();
                    rt.git.diffs.insert(
                        tab,
                        DiffTab {
                            workdir,
                            path: entry.path.clone(),
                            side,
                            request: 0,
                            diff: None,
                        },
                    );
                    (tab, None)
                }
            }
        };
        self.git_panel.blur();
        self.focus_group(existing.unwrap_or(self.focused_group.get()));
        self.activate(tab);
        self.request_diffs(Some(tab));
        self.changed();
    }
    /// Reads the changes shown in diff tabs again (all, or one).
    fn request_diffs(&self, only: Option<TabId>) {
        let mut rt = self.runtime.lock().unwrap();
        let state = &mut rt.git;
        let sender = state
            .diff_answers
            .get_or_insert_with(mpsc::channel)
            .0
            .clone();
        let tabs: Vec<TabId> = state
            .diffs
            .keys()
            .copied()
            .filter(|tab| only.is_none_or(|only| only == *tab))
            .collect();
        for tab in tabs {
            state.diff_requests += 1;
            let request = state.diff_requests;
            let diff = state.diffs.get_mut(&tab).unwrap();
            diff.request = request;
            let (workdir, path, side) = (diff.workdir.clone(), diff.path.clone(), diff.side);
            let sender = sender.clone();
            let spawned = thread::Builder::new()
                .name("kureha-git-diff".into())
                .stack_size(2 * 1024 * 1024)
                .spawn(move || {
                    let _ = sender.send((tab, request, diff::read(&workdir, &path, side)));
                });
            if let Err(e) = spawned {
                diff.diff = Some(Err(format!("Cannot read the change: {e}")));
            }
        }
    }
    fn poll_diffs(&self) {
        let changed = {
            let mut rt = self.runtime.lock().unwrap();
            let state = &mut rt.git;
            let Some((_, answers)) = &state.diff_answers else {
                return;
            };
            let answers: Vec<_> = answers.try_iter().collect();
            let mut changed = false;
            for (tab, request, result) in answers {
                if let Some(diff) = state.diffs.get_mut(&tab)
                    && diff.request == request
                {
                    let result = result.map(Arc::new);
                    let same = match (&diff.diff, &result) {
                        (Some(Ok(a)), Ok(b)) => a == b,
                        (Some(Err(a)), Err(b)) => a == b,
                        _ => false,
                    };
                    if !same {
                        diff.diff = Some(result);
                        changed = true;
                    }
                }
            }
            changed
        };
        if changed {
            self.changed();
        }
    }
    pub(super) fn close_git_diff_tab(&self, id: TabId) -> bool {
        if !self.is_git_diff_tab(id) {
            return false;
        }
        if self.blocked() {
            return true;
        }
        let group = self.focused_group.get();
        let next = {
            let mut rt = self.runtime.lock().unwrap();
            rt.git.diffs.remove(&id);
            let tabs = &mut rt.group_tabs[group];
            let Some(index) = tabs.iter().position(|tab| *tab == id) else {
                return true;
            };
            tabs.remove(index);
            tabs.get(index.min(tabs.len().saturating_sub(1))).copied()
        };
        if self.group_id(group) == Some(id) {
            self.panes.update(|panes| panes[group] = next);
            if next.is_none() {
                self.retire_empty_group(group);
            }
            self.focus_group_terminal(self.focused_group.get());
        }
        self.invalidate_clipboard();
        self.update_position();
        self.changed();
        true
    }

    /// The status bar's Git items: the branch, which opens the branch list,
    /// and the change count, which opens the Git sidebar. Running stages,
    /// commits and switches show as progress beside them.
    pub(super) fn git_status_bar(&self) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let (branch, changes, repository) = {
            let rt = self.runtime.lock().unwrap();
            match rt.git.snapshot.as_ref() {
                Some(snapshot) => (
                    snapshot.head.label(),
                    match snapshot.total {
                        0 => String::new(),
                        1 => "1 change".into(),
                        n => format!("{n} changes"),
                    },
                    true,
                ),
                // Empty outside a repository; `Git: …` when reading failed.
                None => (self.git_status.get(), String::new(), false),
            }
        };
        let clicked = |event: &Event| {
            matches!(
                event,
                Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed {
                    button: scarlet_ui::event::MouseButton::Left,
                    ..
                })
            )
        };
        let picker = self.clone();
        let sidebar = self.clone();
        hstack! {
            Text::new(branch).font_size(11.0).color(colors.foreground)
                .on_event(move |event| {
                    if clicked(event) {
                        if repository { picker.show_branch_picker() } else { picker.show_git() }
                        return true;
                    }
                    false
                }),
            if_view!(!changes.is_empty(),
                Text::new(changes).font_size(11.0).color(colors.muted)
                    .on_event(move |event| {
                        if clicked(event) {
                            sidebar.show_git();
                            return true;
                        }
                        false
                    }),
                VStack::new(())),
        }
        .spacing(10.0)
    }

    /// The Git sidebar.
    pub(super) fn git_view(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let (snapshot, pending, unavailable) = {
            let rt = self.runtime.lock().unwrap();
            (rt.git.snapshot.clone(), rt.git.pending, rt.git.unavailable.clone())
        };
        let refresh = self.clone();
        let commit = self.clone();
        let commit_key = self.clone();
        let branches = self.clone();

        let rows = self.clone();
        let focus = self.git_panel.clone();
        let message_focus = self.git_panel.clone();
        let staged = snapshot.as_ref().map_or(0, |s| s.staged.len());
        let head = snapshot
            .as_ref()
            .map(|s| s.head.label())
            .unwrap_or_default();
        let notice = self.git_panel.notice.get();
        // Kept apart from the notice so a writer's answer never hides it.
        let warning = if let Some(state) = snapshot.as_ref().and_then(|s| s.in_progress.as_ref()) {
            format!("Git is {state}")
        } else {
            String::new()
        };
        self.nav.row_inputs.retain(
            "git:",
            self.git_panel.rows.get().iter().map(GitRow::key),
        );
        let empty = unavailable.unwrap_or_else(|| match self.git_status.get() {
            status if status.starts_with("Git: ") => status,
            _ if self.runtime.lock().unwrap().explorer.root.is_none() => "No folder open".into(),
            _ => "Not a Git repository".into(),
        });
        let header = hstack! {
            Text::new("Source Control").font_size(13.0).color(colors.foreground),
            Spacer::new(),
            Button::icon_only(Icon::Refresh).header_style().text_color(colors.foreground)
                .background_color(Color::TRANSPARENT).padding(5.0)
                .on_click(move || refresh.refresh_git()),
        }
        .spacing(2.0);
        let discard = self.clone();
        let cancel_discard = self.git_panel.discard.clone();
        let discard_target = self.git_panel.discard.get().filter(|(root, _)| snapshot.as_ref().is_some_and(|s| &s.workdir == root));
        let discard_label = discard_target.as_ref().map_or(String::new(), |(_, entry)| format!("Discard changes to {}? Staged changes are kept.", entry.path.display()));
        let row_theme = theme.clone();
        let body = if_view!(snapshot.is_some(),
            vstack! {
                Button::new(head).header_style()
                    .icon(Icon::ChevronDown).icon_size(IconSize::Small)
                    .font_size(12.0).padding(4.0).text_color(colors.foreground)
                    .background_color(Color::TRANSPARENT)
                    .on_click(move || branches.show_branch_picker()),
                TextView::new(self.git_panel.message.clone(), self.git_panel.message_selection.clone())
                    .placeholder(if cfg!(target_os = "macos") {
                        "Message (⌘Enter to commit)"
                    } else {
                        "Message (Ctrl+Enter to commit)"
                    })
                    .font_size(13.0).padding(6.0)
                    .background(colors.elevated).text_color(colors.foreground)
                    .placeholder_color(colors.muted).border_color(colors.border)
                    .focused_border_color(colors.accent)
                    .material(theme.input_material())
                    .wrap_mode(scarlet_ui::views::WrapMode::Soft)
                    .autofocus(self.git_wants_focus() && self.git_panel.focus_message.get())
                    .frame(f32::INFINITY, 76.0)
                    .on_event_capture(move |event| {
                        if matches!(event, Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })) {
                            message_focus.focus_message.set(true);
                        }
                        false
                    }),
                Button::new(if staged == 0 { "Commit (nothing staged)".to_string() } else { format!("Commit {staged} staged") })
                    .icon(Icon::Check).icon_size(IconSize::Small)
                    .font_size(12.0).text_color(colors.foreground).material(theme.input_material())
                    .enabled(staged > 0 && pending == 0)
                    .on_click(move || commit.git_commit())
                    .frame(f32::INFINITY, 28.0),
                if_view!(!warning.is_empty(),
                    Text::new(warning.clone()).font_size(11.0).color(colors.warning).line_limit(None),
                    VStack::new(())),
                if_view!(!notice.is_empty(),
                    Text::new(notice).font_size(11.0).color(colors.error).line_limit(None),
                    VStack::new(())),
                if_view!(discard_target.is_some(),
                    vstack! {
                        Text::new(discard_label).font_size(12.0).color(colors.foreground).line_limit(None),
                        hstack! {
                            Button::new("Cancel").font_size(12.0).on_click(move || cancel_discard.set(None)),
                            Button::new("Discard Changes").font_size(12.0).text_color(colors.error).enabled(pending == 0).on_click(move || discard.confirm_git_discard()),
                        }.spacing(8.0),
                    }.spacing(8.0).padding(10.0).background(colors.elevated),
                    VStack::new(())),
                ListView::new(self.git_panel.rows.clone(), self.git_panel.selected.clone(), 27.0, move |index, row: GitRow, selected| {
                    rows.git_row(row, selected == Some(index), &row_theme)
                }).item_key(|row| row.key().into())
                  .frame(f32::INFINITY, f32::INFINITY),
            }.alignment(Alignment::Leading).spacing(8.0),
            vstack! {
                Text::new(empty).font_size(12.0).color(colors.muted).line_limit(None),
                Spacer::new(),
            }.alignment(Alignment::Leading)
        );
        vstack! { header, body }
            .alignment(Alignment::Leading)
            .spacing(8.0)
            .padding_insets(EdgeInsets::new(12.0, 4.0, 12.0, 12.0))
            .frame(f32::INFINITY, f32::INFINITY)
            .on_event_capture(move |event| {
                if matches!(
                    event,
                    Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })
                ) {
                    focus.owns_focus.set(true);
                    focus.focus_message.set(false);
                }
                // Enter adds a line to the message; with the primary key it commits.
                if let Event::Keyboard(KeyEvent::Pressed {
                    keycode: KeyCode::Enter,
                    modifiers,
                }) = event
                    && modifiers.primary()
                    && focus.focus_message.get()
                {
                    commit_key.git_commit();
                    return true;
                }
                false
            })
    }

    fn git_row(
        &self,
        row: GitRow,
        selected: bool,
        theme: &kureha_code::theme::Theme,
    ) -> impl View + Clone + use<> {
        let colors = theme.colors();
        let action = self.clone();
        let open = self.clone();
        let input = self.nav.row_inputs.get(format!("git:{}", row.key()));
        match row {
            GitRow::Header { staged, count } => {
                let entries: Vec<Entry> = {
                    let rt = self.runtime.lock().unwrap();
                    rt.git
                        .snapshot
                        .as_ref()
                        .map(|s| {
                            if staged {
                                s.staged.clone()
                            } else {
                                s.unstaged.clone()
                            }
                        })
                        .unwrap_or_default()
                };
                Either::A(
                    hstack! {
                        Text::new(format!("{}  {count}", if staged { "STAGED" } else { "CHANGES" }))
                            .font_size(11.0).color(colors.muted),
                        Spacer::new(),
                        Button::new(if staged { "Unstage All" } else { "Stage All" }).header_style()
                            .font_size(11.0).padding(4.0).text_color(colors.foreground)
                            .background_color(Color::TRANSPARENT)
                            .on_click(move || action.git_stage(entries.clone(), staged)),
                    }
                    .spacing(4.0)
                    .padding_insets(EdgeInsets::new(6.0, 2.0, 0.0, 2.0))
                    .frame(f32::INFINITY, 27.0),
                )
            }
            GitRow::File { entry, staged } => {
                let path = entry.path.clone();
                let name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |n| n.to_string_lossy().into_owned(),
                );
                let name = if entry.folder {
                    format!("{name}/")
                } else {
                    name
                };
                let parent = path
                    .parent()
                    .map(|p| p.display().to_string())
                    .unwrap_or_default();
                let color = change_color(entry.change, &colors);
                let icon = if entry.folder {
                    file_icons::folder(false)
                } else {
                    file_icons::for_path(Some(&path))
                };
                // The selected entry's name in the accent; its letter keeps
                // the change's colour.
                let name_color = if selected {
                    colors.emphasis(colors.elevated)
                } else {
                    color
                };
                let target = entry.clone();
                let stage = entry.clone();
                let discard = self.clone();
                let discardable = !staged && !entry.folder && matches!(entry.change, Change::Modified | Change::Deleted);
                let discard_entry = entry.clone();
                Either::B(
                    hstack! {
                        // One flat row, so the name and folder shorten before the letter.
                        super::ui::list_row(hstack! {
                            Spacer::new().frame(4.0, 1.0),
                            icon.view(),
                            Spacer::new().frame(6.0, 1.0),
                            Text::new(name).font_size(13.0).color(name_color),
                            Spacer::new().frame(6.0, 1.0),
                            Text::new(parent).font_size(11.0).color(colors.muted)
                                .truncation(Truncation::Middle),
                            Spacer::new(),
                            Text::new(entry.change.letter()).font_size(11.0).color(color)
                                .padding_insets(EdgeInsets::new(0.0, 0.0, 6.0, 0.0)),
                        }.spacing(0.0).frame(f32::INFINITY, 27.0).clip(),
                        input, selected, !entry.folder, colors,
                        move || open.open_git_diff(&target, staged)),
                        if_view!(discardable,
                            Button::new("↶").header_style().font_size(15.0).padding(4.0)
                                .text_color(colors.foreground).background_color(Color::TRANSPARENT)
                                .enabled(!self.git_panel.busy.get())
                                .on_click(move || discard.request_git_discard(discard_entry.clone()))
                                .frame(26.0, 27.0),
                            VStack::new(())),
                        Button::new(if staged { "−" } else { "+" }).header_style()
                            .font_size(13.0).padding(4.0).text_color(colors.foreground)
                            .background_color(Color::TRANSPARENT)
                            .on_click(move || action.git_stage(vec![stage.clone()], staged))
                            .frame(26.0, 27.0),
                    }
                    .spacing(2.0)
                    .frame(f32::INFINITY, 27.0),
                )
            }
        }
    }

    /// A diff tab's content.
    pub(super) fn git_diff_view(&self, tab: TabId) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let (title, diff) = {
            let rt = self.runtime.lock().unwrap();
            match rt.git.diffs.get(&tab) {
                Some(d) => (
                    format!(
                        "{} · {}",
                        d.path.display(),
                        match d.side {
                            Side::Staged => "staged (HEAD → index)",
                            Side::Unstaged => "changes (index → work tree)",
                        }
                    ),
                    d.diff.clone(),
                ),
                None => (String::new(), None),
            }
        };
        let (summary, message, lines) = match diff {
            None => (String::new(), "Reading…".to_string(), None),
            Some(Err(e)) => (String::new(), e, None),
            Some(Ok(diff)) => {
                let summary = format!("+{} −{}", diff.added, diff.removed);
                let message = match (&diff.note, diff.lines.is_empty()) {
                    (Some(note), _) => note.clone(),
                    (None, true) => "No differences".into(),
                    (None, false) => String::new(),
                };
                let lines = (!diff.lines.is_empty()).then_some(diff);
                (summary, message, lines)
            }
        };
        let body = match lines {
            Some(diff) => Either::A(
                ScrollView::new(crate::git_diff_view::DiffLines {
                    diff,
                    theme: theme.clone(),
                    font: self.document_font(),
                })
                .scrollbar_color(colors.muted)
                .frame(f32::INFINITY, f32::INFINITY)
                .clip()
                .key(format!("git-diff-{tab}")),
            ),
            None => Either::B(VStack::new(()).frame(f32::INFINITY, f32::INFINITY)),
        };
        Surface::section(
            vstack! {
                hstack! {
                    Text::new(title).font_size(12.0).color(colors.foreground)
                        .truncation(Truncation::Middle),
                    Spacer::new(),
                    Text::new(summary).font_size(12.0).color(colors.muted),
                }.spacing(8.0).padding_insets(EdgeInsets::new(14.0, 10.0, 14.0, 4.0)),
                if_view!(!message.is_empty(),
                    Text::new(message.clone()).font_size(12.0).color(colors.muted).line_limit(None)
                        .padding_insets(EdgeInsets::new(14.0, 4.0, 14.0, 4.0)),
                    VStack::new(())),
                body,
            }
            .alignment(Alignment::Leading)
            .spacing(0.0)
            .frame(f32::INFINITY, f32::INFINITY),
        )
        .fill(theme.editor_colors().background)
        .corner_radius(12.0)
        .clip_content(true)
        .padding_insets(EdgeInsets::new(4.0, 0.0, 4.0, 4.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kureha_code::git::Change;
    use std::process::Command;

    #[test]
    fn missing_git_disables_operations_without_repeated_errors() {
        const CHILD: &str = "KUREHA_TEST_GIT_UI_MISSING";
        if std::env::var_os(CHILD).is_none() {
            let output = Command::new(std::env::current_exe().unwrap())
                .args(["--exact", "git_ui::tests::missing_git_disables_operations_without_repeated_errors", "--nocapture"])
                .env(CHILD, "1").env("PATH", "").output().unwrap();
            assert!(output.status.success(), "{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
            return;
        }
        let app = App::with_document();
        app.open_folder(std::env::current_dir().unwrap());
        app.show_git();
        wait(&app, "Git is disabled", |app| app.runtime.lock().unwrap().git.unavailable.is_some());
        for _ in 0..3 { app.refresh_git(); app.poll_git(); }
        app.show_branch_picker();
        app.git_commit();
        assert!(!app.git_can_operate());
        assert!(app.runtime.lock().unwrap().git.watcher.is_none());
        assert!(app.runtime.lock().unwrap().git.writer.is_none());
        assert!(app.git_status.get().is_empty());
        assert!(app.git_panel.rows.get().is_empty());
        assert!(!app.git_panel.busy.get());
        assert!(app.messages.progress.get().is_empty());
        assert!(!app.nav.is_open());
    }

    fn git(dir: &std::path::Path, args: &[&str]) {
        let output = Command::new("git")
            .args([
                "-c",
                "user.name=Test",
                "-c",
                "user.email=test@example.invalid",
            ])
            .args(args)
            .current_dir(dir)
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .output()
            .expect("git is on PATH for these tests");
        assert!(output.status.success(), "git {args:?}");
    }
    fn wait(app: &App, what: &str, done: impl Fn(&App) -> bool) {
        let until = Instant::now() + Duration::from_secs(20);
        while !done(app) {
            assert!(
                Instant::now() < until,
                "{what}; git status: {}",
                app.git_status.get()
            );
            app.poll();
            app.poll_git();
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    /// Writes a PNG of the workbench when `KUREHA_WORKBENCH_PREVIEW` names a folder.
    fn render_preview(app: &App, name: &str) {
        render_preview_sized(app, name, 1100.0);
    }
    /// The workbench at `width`; the sidebar takes its minimum below ~860.
    fn render_preview_sized(app: &App, name: &str, width: f32) {
        use scarlet_ui::{pipeline::RenderingPipeline, prelude::*};
        let Some(directory) = std::env::var_os("KUREHA_WORKBENCH_PREVIEW") else {
            return;
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Kureha Code", app.content())
                .decorated(false)
                .size(Size::new(width, 560.0))
                .create_element(),
        );
        pipeline.layout_initial();
        let buffer = pipeline.render().expect("workbench paints");
        let pixels = buffer
            .data()
            .chunks_exact(4)
            .flat_map(|p| [p[2], p[1], p[0]])
            .collect();
        image::RgbImage::from_raw(buffer.width(), buffer.height(), pixels)
            .unwrap()
            .save(PathBuf::from(directory).join(name))
            .unwrap();
        pipeline.teardown();
    }
    fn mark(app: &App, label: &str) -> Option<Change> {
        app.tree_rows
            .get()
            .into_iter()
            .find(|row| row.label == label)
            .unwrap_or_else(|| panic!("{label} is listed"))
            .git
    }

    #[test]
    fn the_status_bar_and_explorer_show_the_repository() {
        let dir = std::env::temp_dir().join(format!("kureha-git-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        for file in [
            "README.md",
            "keep.txt",
            "src/lib.rs",
            "a_rather_long_file_name_for_a_narrow_explorer.txt",
        ] {
            std::fs::write(dir.join(file), "one\n").unwrap();
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(dir.join("src/lib.rs"), "two\n").unwrap();
        std::fs::write(dir.join("notes.txt"), "new\n").unwrap();

        let app = App::with_document();
        app.open_folder(dir.clone());
        wait(&app, "the folder opens", |app| {
            app.tree_rows
                .get()
                .iter()
                .any(|row| row.label == "notes.txt")
        });
        wait(&app, "the repository is read", |app| {
            app.git_status.get() == "main · 2 changes"
        });
        assert_eq!(mark(&app, "notes.txt"), Some(Change::Untracked));
        assert_eq!(mark(&app, "src"), Some(Change::Modified));
        assert_eq!(mark(&app, "keep.txt"), None);

        // A change made outside the editor appears on the next read.
        std::fs::write(dir.join("keep.txt"), "two\n").unwrap();
        app.refresh_git();
        wait(&app, "the change is noticed", |app| {
            app.git_status.get() == "main · 3 changes"
        });
        assert_eq!(mark(&app, "keep.txt"), Some(Change::Modified));
        render_preview(&app, "git-explorer.png");
        render_preview_sized(&app, "git-explorer-narrow.png", 760.0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn discard_requires_confirmation_preserves_dirty_buffers_and_reloads_clean_documents() {
        let dir = std::env::temp_dir().join(format!("kureha-git-discard-ui-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        std::fs::write(dir.join("file.txt"), "original\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-qm", "initial"]);
        std::fs::write(dir.join("file.txt"), "changed\n").unwrap();
        let app = App::with_document();
        app.open_folder(dir.clone());
        wait(&app, "status", |app| app.runtime.lock().unwrap().git.snapshot.as_ref().is_some_and(|s| s.unstaged.len() == 1));
        let entry = app.runtime.lock().unwrap().git.snapshot.as_ref().unwrap().unstaged[0].clone();
        app.request_git_discard(entry.clone());
        assert_eq!(std::fs::read_to_string(dir.join("file.txt")).unwrap(), "changed\n");
        app.git_panel.discard.set(None);
        app.confirm_git_discard();
        assert_eq!(std::fs::read_to_string(dir.join("file.txt")).unwrap(), "changed\n");
        app.open(dir.join("file.txt"));
        wait(&app, "file opens", |app| app.active_id().is_some_and(|id| app.runtime.lock().unwrap().workspace.get(id).unwrap().path.as_ref().is_some_and(|p| p.ends_with("file.txt"))));
        let id = app.active_id().unwrap();
        app.edit(id, &EditDelta { replaced_range: 0..0, inserted: "unsaved".into() });
        app.request_git_discard(entry.clone());
        app.confirm_git_discard();
        assert!(app.git_panel.notice.get().contains("unsaved"));
        assert_eq!(std::fs::read_to_string(dir.join("file.txt")).unwrap(), "changed\n");
        app.undo(false);
        app.request_git_discard(entry);
        app.confirm_git_discard();
        wait(&app, "discard reloads editor", |app| app.runtime.lock().unwrap().workspace.get(id).unwrap().editor.document.as_str().as_ref() == "original\n");
        assert_eq!(std::fs::read_to_string(dir.join("file.txt")).unwrap(), "original\n");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_git_sidebar_stages_commits_and_shows_diffs() {
        let dir = std::env::temp_dir().join(format!("kureha-git-panel-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("src")).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        // Kureha reads the repository's config: an identity, no signing.
        git(&dir, &["config", "user.name", "Test"]);
        git(&dir, &["config", "user.email", "test@example.invalid"]);
        git(&dir, &["config", "commit.gpgsign", "false"]);
        let body: String = (1..=12).map(|i| format!("fn f{i}() {{}}\n")).collect();
        std::fs::write(dir.join("src/lib.rs"), &body).unwrap();
        std::fs::write(dir.join("README.md"), "# Demo\n").unwrap();
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "init"]);
        std::fs::write(
            dir.join("src/lib.rs"),
            body.replace("fn f6() {}", "fn f6() {\n    todo!()\n}"),
        )
        .unwrap();
        std::fs::write(dir.join("notes.txt"), "new\n").unwrap();
        let deep = dir.join("src/components/forms/inputs");
        std::fs::create_dir_all(&deep).unwrap();
        std::fs::write(deep.join("a_rather_long_component_name.rs"), "new\n").unwrap();
        git(&dir, &["add", "-N", "src/components"]);

        let app = App::with_document();
        app.open_folder(dir.clone());
        app.show_git();
        let file = |app: &App, name: &str, want_staged: bool| {
            app.git_panel
                .rows
                .get()
                .into_iter()
                .find_map(|row| match row {
                    GitRow::File { entry, staged }
                        if staged == want_staged && entry.path.ends_with(name) =>
                    {
                        Some(entry)
                    }
                    _ => None,
                })
        };
        wait(&app, "the changes are listed", |app| {
            file(app, "notes.txt", false).is_some() && file(app, "lib.rs", false).is_some()
        });

        app.open_git_diff(&file(&app, "lib.rs", false).unwrap(), false);
        let tab = app.group_id(app.focused_group.get()).unwrap();
        assert!(app.is_git_diff_tab(tab));
        wait(&app, "the change is read", |app| {
            let rt = app.runtime.lock().unwrap();
            matches!(&rt.git.diffs[&tab].diff, Some(Ok(d)) if (d.added, d.removed) == (3, 1))
        });
        render_preview(&app, "git-diff.png");

        let unstaged = app
            .runtime
            .lock()
            .unwrap()
            .git
            .snapshot
            .as_ref()
            .unwrap()
            .unstaged
            .clone();
        app.git_stage(unstaged, false);
        wait(&app, "both files are staged", |app| {
            file(app, "notes.txt", true).is_some()
                && file(app, "lib.rs", true).is_some()
                && file(app, "lib.rs", false).is_none()
        });
        assert!(app.git_panel.notice.get().is_empty());
        assert!(app.messages.progress.get().is_empty());
        render_preview(&app, "git-sidebar.png");
        render_preview_sized(&app, "git-sidebar-narrow.png", 760.0);

        app.git_panel.message.set("Fill in f6".into());
        app.git_commit();
        wait(&app, "the commit is made", |app| {
            app.git_status.get() == "main" && !app.git_panel.busy.get()
        });
        assert!(app.git_panel.notice.get().is_empty());
        assert!(app.messages.progress.get().is_empty());
        assert!(!app.messages.history().iter().any(|m| m.text.starts_with("Committed ")));
        assert_eq!(app.git_panel.message.get(), "");
        assert!(app.git_panel.rows.get().is_empty());
        let log = Command::new("git")
            .args(["log", "-1", "--format=%s"])
            .current_dir(&dir)
            .output()
            .unwrap();
        assert_eq!(String::from_utf8_lossy(&log.stdout), "Fill in f6\n");

        app.request_close(Some(tab));
        assert!(!app.is_git_diff_tab(tab));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[cfg(unix)]
    #[test]
    fn switching_branches_reloads_open_files_and_keeps_unsaved_ones() {
        let dir = std::env::temp_dir().join(format!("kureha-git-branch-ui-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        git(&dir, &["init", "-q", "-b", "main"]);
        git(&dir, &["config", "user.name", "Test"]);
        git(&dir, &["config", "user.email", "test@example.invalid"]);
        for (file, text) in [("a.txt", "main\n"), ("gone.txt", "only on main\n"), ("draft.txt", "d\n")] {
            std::fs::write(dir.join(file), text).unwrap();
        }
        git(&dir, &["add", "."]);
        git(&dir, &["commit", "-q", "-m", "main"]);
        git(&dir, &["switch", "-q", "-c", "side"]);
        std::fs::write(dir.join("a.txt"), "side\n").unwrap();
        git(&dir, &["rm", "-q", "gone.txt"]);
        git(&dir, &["commit", "-q", "-am", "side"]);
        git(&dir, &["switch", "-q", "main"]);

        let app = App::with_document();
        app.open_folder(dir.clone());
        wait(&app, "the repository is read", |app| app.git_status.get() == "main");
        assert_eq!(
            app.git_branches().iter().map(|b| (b.name.as_str(), b.current)).collect::<Vec<_>>(),
            [("main", true), ("side", false)]
        );
        let open = |name: &str| {
            let path = dir.join(name);
            let text = std::fs::read_to_string(&path).unwrap();
            let id = app.runtime.lock().unwrap().workspace.open(path, &text);
            app.activate(id);
            id
        };
        let a = open("a.txt");
        let gone = open("gone.txt");
        let draft = open("draft.txt");
        app.runtime.lock().unwrap().workspace.get_mut(draft).unwrap().editor.edit(
            &scarlet_ui::views::EditDelta { replaced_range: 0..0, inserted: "unsaved ".into() },
        );

        app.switch_branch("side".into());
        wait(&app, "the switch is done", |app| {
            app.git_status.get() == "side" && !app.git_panel.busy.get()
        });
        assert!(app.git_panel.notice.get().is_empty());
        assert!(app.messages.progress.get().is_empty());
        assert!(!app.messages.history().iter().any(|m| m.text.starts_with("Switched to ")));
        let rt = app.runtime.lock().unwrap();
        let doc = rt.workspace.get(a).unwrap();
        assert_eq!(doc.editor.document.as_str(), "side\n");
        assert!(!doc.editor.dirty());
        assert!(rt.workspace.get(gone).is_none(), "the deleted file's tab closes");
        assert!(rt.workspace.get(draft).unwrap().editor.dirty(), "unsaved edits stay");
        drop(rt);

        // Unsaved edits to a file the switch would change stop it.
        app.runtime.lock().unwrap().workspace.get_mut(a).unwrap().editor.edit(
            &scarlet_ui::views::EditDelta { replaced_range: 0..0, inserted: "x".into() },
        );
        app.switch_branch("main".into());
        wait(&app, "the switch is refused", |app| !app.git_panel.busy.get() && app.status.get().starts_with("Save or close"));
        assert_eq!(app.git_status.get(), "side");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
