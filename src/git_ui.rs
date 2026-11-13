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
use scarlet_ui::prelude::State as Ui;
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
    pub(super) open: Ui<bool>,
    owns_focus: Ui<bool>,
    focus_message: Ui<bool>,
    rows: Ui<Vec<GitRow>>,
    selected: Ui<Option<usize>>,
    message: Ui<String>,
    /// The last answer from the writer, or why nothing can be done.
    notice: Ui<String>,
    busy: Ui<bool>,
}
impl Panel {
    pub(super) fn new() -> Self {
        Self {
            open: Ui::new(generate_state_id(), false),
            owns_focus: Ui::new(generate_state_id(), false),
            focus_message: Ui::new(generate_state_id(), false),
            rows: Ui::new(generate_state_id(), Vec::new()),
            selected: Ui::new(generate_state_id(), None),
            message: Ui::new(generate_state_id(), String::new()),
            notice: Ui::new(generate_state_id(), String::new()),
            busy: Ui::new(generate_state_id(), false),
        }
    }
    pub(super) fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![
            &self.open,
            &self.owns_focus,
            &self.focus_message,
            &self.rows,
            &self.notice,
            &self.busy,
        ]
    }
    /// The sidebar holds keyboard focus, so the editor must not take it.
    pub(super) fn wants_focus(&self) -> bool {
        self.open.get() && self.owns_focus.get()
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
        Outcome::NotRepository => String::new(),
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
        self.project.close();
        self.git_panel.open.set(true);
        self.refresh_git();
    }
    /// Shows the explorer.
    pub(super) fn show_explorer(&self) {
        self.project.close();
        self.git_panel.open.set(false);
        self.git_panel.blur();
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
                self.git_panel.busy.set(true);
                self.git_panel.notice.set(
                    match operation {
                        Operation::Stage(_) => "Staging…",
                        Operation::Unstage(_) => "Unstaging…",
                        Operation::Commit(_) => "Committing…",
                    }
                    .into(),
                );
            }
            Err(e) => self.git_panel.notice.set(e),
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
            match result {
                Ok(text) => {
                    if matches!(operation, Operation::Commit(_)) {
                        self.git_panel.message.set(String::new());
                    }
                    self.git_panel.notice.set(text);
                }
                Err(e) => self.git_panel.notice.set(e),
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

    /// The switcher above the sidebar: explorer, search, Git.
    pub(super) fn sidebar_switcher(&self) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let current = if self.project.is_open() {
            1
        } else if self.git_panel.open.get() {
            2
        } else {
            0
        };
        let button = |icon: Icon, index: usize, action: Box<dyn Fn()>| {
            Button::icon_only(icon)
                .header_style()
                .text_color(if index == current {
                    colors.foreground
                } else {
                    colors.muted
                })
                .background_color(if index == current {
                    colors.selection
                } else {
                    Color::TRANSPARENT
                })
                .padding(6.0)
                .on_click(action)
        };
        let explorer = self.clone();
        let search = self.clone();
        let git = self.clone();
        hstack! {
            button(Icon::Folder, 0, Box::new(move || explorer.show_explorer())),
            button(Icon::Search, 1, Box::new(move || search.show_project_search())),
            // The pinned icon set has no branch symbol yet.
            button(Icon::Share, 2, Box::new(move || git.show_git())),
            Spacer::new(),
        }
        .spacing(4.0)
        .padding_insets(EdgeInsets::new(12.0, 10.0, 8.0, 2.0))
    }

    /// The Git sidebar.
    pub(super) fn git_view(&self) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let colors = theme.colors();
        let (snapshot, pending) = {
            let rt = self.runtime.lock().unwrap();
            (rt.git.snapshot.clone(), rt.git.pending)
        };
        let refresh = self.clone();
        let commit = self.clone();
        let submit = self.clone();
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
            format!("Git is {state}; finish it with git")
        } else if snapshot.as_ref().is_some_and(|s| s.hooks) {
            "This repository has commit hooks; Kureha does not run them".into()
        } else {
            String::new()
        };
        self.nav.row_inputs.retain(
            "git:",
            self.git_panel.rows.get().iter().map(GitRow::key),
        );
        let empty = match self.git_status.get() {
            status if status.starts_with("Git: ") => status,
            _ if self.runtime.lock().unwrap().explorer.root.is_none() => "No folder open".into(),
            _ => "Not a Git repository".into(),
        };
        let header = hstack! {
            Text::new("Source Control").font_size(13.0).color(colors.foreground),
            Spacer::new(),
            Button::icon_only(Icon::Refresh).header_style().text_color(colors.foreground)
                .background_color(Color::TRANSPARENT).padding(5.0)
                .on_click(move || refresh.refresh_git()),
        }
        .spacing(2.0);
        let row_theme = theme.clone();
        let body = if_view!(snapshot.is_some(),
            vstack! {
                Text::new(head).font_size(12.0).color(colors.muted),
                // One line for now: a multi-line TextView overflows the sidebar
                // (it measures 400 px wide there).
                TextField::new(self.git_panel.message.clone()).font_size(13.0).padding(5.0)
                    .background_color(colors.elevated).text_color(colors.foreground)
                    .placeholder_color(colors.muted).border_color(colors.border)
                    .material(theme.input_material())
                    .placeholder("Commit message (Enter to commit)")
                    .autofocus(self.git_panel.wants_focus() && self.git_panel.focus_message.get())
                    .on_submit(move || submit.git_commit())
                    .frame(f32::INFINITY, 28.0)
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
                    Text::new(warning.clone()).font_size(11.0).color(colors.warning),
                    VStack::new(())),
                Text::new(notice).font_size(11.0).color(colors.muted),
                ListView::new(self.git_panel.rows.clone(), self.git_panel.selected.clone(), 27.0, move |index, row: GitRow, selected| {
                    rows.git_row(row, selected == Some(index), &row_theme)
                }).item_key(|row| row.key().into())
                  .frame(f32::INFINITY, f32::INFINITY),
            }.alignment(Alignment::Leading).spacing(8.0),
            vstack! {
                Text::new(empty).font_size(12.0).color(colors.muted),
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
                }
                .themed(theme, selected);
                let target = entry.clone();
                let stage = entry.clone();
                Either::B(
                    hstack! {
                        super::ui::list_row(hstack! {
                            hstack! {
                                icon.view(),
                                Text::new(name).font_size(13.0).color(color),
                                Text::new(parent).font_size(11.0).color(colors.muted),
                            }.spacing(6.0).padding(4.0),
                            Spacer::new(),
                            Text::new(entry.change.letter()).font_size(11.0).color(color)
                                .padding_insets(EdgeInsets::new(0.0, 0.0, 6.0, 0.0)),
                        }.spacing(0.0).frame(f32::INFINITY, 27.0).clip(),
                        input, selected, !entry.folder, colors,
                        move || open.open_git_diff(&target, staged)),
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
                    Text::new(title).font_size(12.0).color(colors.foreground),
                    Spacer::new(),
                    Text::new(summary).font_size(12.0).color(colors.muted),
                }.spacing(8.0).padding_insets(EdgeInsets::new(14.0, 10.0, 14.0, 4.0)),
                if_view!(!message.is_empty(),
                    Text::new(message.clone()).font_size(12.0).color(colors.muted)
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
        use scarlet_ui::{pipeline::RenderingPipeline, prelude::*};
        let Some(directory) = std::env::var_os("KUREHA_WORKBENCH_PREVIEW") else {
            return;
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Kureha Code", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 560.0))
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
        for file in ["README.md", "keep.txt", "src/lib.rs"] {
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
        let _ = std::fs::remove_dir_all(&dir);
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
        assert_eq!(app.git_panel.notice.get(), "Staged 2 files");
        render_preview(&app, "git-sidebar.png");

        app.git_panel.message.set("Fill in f6".into());
        app.git_commit();
        wait(&app, "the commit is made", |app| {
            app.git_status.get() == "main" && !app.git_panel.busy.get()
        });
        assert!(app.git_panel.notice.get().starts_with("Committed "));
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
}
