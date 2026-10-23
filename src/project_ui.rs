//! Project search owns its worker and immutable result snapshots. Replacement
//! prepares edits off-thread, then preflights every source before changing any
//! document. Disk writes remain the ordinary, conflict-checked Save operation.
use super::*;
use kureha_code::{
    navigation,
    project_search::{self, Replacement, Results, Source},
    search::Options,
};
use std::{collections::BTreeMap, ops::Range, result::Result, sync::atomic::AtomicBool};

#[derive(Clone)]
struct ResultRow {
    key: String,
    hit: usize,
    location: String,
    preview: String,
}

#[derive(Clone)]
struct Context {
    root: PathBuf,
    root_generation: u64,
    input_revision: u64,
}

struct Found {
    context: Context,
    results: Arc<Results>,
}

enum Output {
    Search(Arc<Results>, String),
    Replace(Vec<Replacement>, Arc<Results>),
}

struct Job {
    context: Context,
    cancel: Arc<AtomicBool>,
    receiver: mpsc::Receiver<Result<Output, String>>,
    replacement: Option<String>,
}

struct PendingNavigation {
    context: Context,
    source: Source,
    range: Range<usize>,
    navigation: u64,
}

#[derive(Default)]
struct SearchRuntime {
    job: Option<Job>,
    found: Option<Found>,
    navigation: Option<PendingNavigation>,
}

#[derive(Clone)]
pub(super) struct ProjectSearch {
    open: State<bool>,
    owns_focus: State<bool>,
    focus_query: State<bool>,
    query: State<String>,
    replacement: State<String>,
    match_case: State<bool>,
    whole_word: State<bool>,
    regex: State<bool>,
    rows: State<Vec<ResultRow>>,
    selected: State<Option<usize>>,
    message: State<String>,
    busy: State<bool>,
    input_revision: Arc<AtomicU64>,
    runtime: Arc<Mutex<SearchRuntime>>,
}

impl ProjectSearch {
    pub(super) fn new() -> Self {
        let search = Self {
            open: State::new(generate_state_id(), false),
            owns_focus: State::new(generate_state_id(), false),
            focus_query: State::new(generate_state_id(), false),
            query: State::new(generate_state_id(), String::new()),
            replacement: State::new(generate_state_id(), String::new()),
            match_case: State::new(generate_state_id(), false),
            whole_word: State::new(generate_state_id(), false),
            regex: State::new(generate_state_id(), false),
            rows: State::initial(generate_state_id()),
            selected: State::initial(generate_state_id()),
            message: State::new(
                generate_state_id(),
                "Search files in the open folder".into(),
            ),
            busy: State::new(generate_state_id(), false),
            input_revision: Arc::new(AtomicU64::new(0)),
            runtime: Arc::new(Mutex::new(SearchRuntime::default())),
        };
        // Replacement text does not invalidate search hits, but each replace job
        // captures its own exact replacement and rejects subsequent changes.
        for state in [
            &search.query as &dyn Listenable,
            &search.match_case,
            &search.whole_word,
            &search.regex,
        ] {
            let revision = search.input_revision.clone();
            state.subscribe_any(Arc::new(move || {
                revision.fetch_add(1, Ordering::Relaxed);
            }));
        }
        search
    }

    pub(super) fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![
            &self.open,
            &self.owns_focus,
            &self.focus_query,
            &self.query,
            &self.replacement,
            &self.match_case,
            &self.whole_word,
            &self.regex,
            &self.rows,
            &self.busy,
            &self.message,
        ]
    }

    pub(super) fn is_open(&self) -> bool {
        self.open.get()
    }

    pub(super) fn wants_focus(&self) -> bool {
        self.open.get() && self.owns_focus.get()
    }

    pub(super) fn blur(&self) {
        self.owns_focus.set(false);
        self.focus_query.set(false);
    }

    pub(super) fn close(&self) {
        self.owns_focus.set(false);
        self.focus_query.set(false);
        self.open.set(false);
        self.runtime.lock().unwrap().navigation = None;
    }

    fn options(&self) -> Options {
        Options {
            match_case: self.match_case.get(),
            whole_word: self.whole_word.get(),
            regex: self.regex.get(),
        }
    }

    fn cancel(&self) {
        let rt = self.runtime.lock().unwrap();
        if let Some(job) = &rt.job {
            job.cancel.store(true, Ordering::Relaxed);
            drop(rt);
            self.message.set("Cancelling…".into());
        }
    }

    fn clear_results(&self) {
        {
            let mut rt = self.runtime.lock().unwrap();
            rt.found = None;
            rt.navigation = None;
        }
        self.rows.set(Vec::new());
        self.selected.set(None);
    }
}

impl App {
    pub(super) fn show_project_search(&self) {
        self.terminal.blur();
        self.find_open.set(false);
        self.dismiss_language_panel();
        self.project.owns_focus.set(true);
        self.project.focus_query.set(true);
        self.project.open.set(true);
    }

    fn project_context(&self) -> Option<Context> {
        let rt = self.runtime.lock().unwrap();
        Some(Context {
            root: rt.explorer.root.clone()?,
            root_generation: rt.explorer.generation,
            input_revision: self.project.input_revision.load(Ordering::Relaxed),
        })
    }

    fn project_context_current(&self, context: &Context) -> bool {
        let rt = self.runtime.lock().unwrap();
        rt.explorer.root.as_ref() == Some(&context.root)
            && rt.explorer.generation == context.root_generation
            && self.project.input_revision.load(Ordering::Relaxed) == context.input_revision
    }

    fn start_project_job(
        &self,
        context: Context,
        replacement: Option<String>,
        work: impl FnOnce(&AtomicBool) -> Result<Output, String> + Send + 'static,
    ) {
        let cancel = Arc::new(AtomicBool::new(false));
        let stopped = cancel.clone();
        let (sender, receiver) = mpsc::channel();
        let mut rt = self.project.runtime.lock().unwrap();
        if rt.job.is_some() {
            return;
        }
        match thread::Builder::new()
            .name("kureha-project-search".into())
            .spawn(move || {
                let _ = sender.send(work(&stopped));
            }) {
            Ok(_) => {
                rt.job = Some(Job {
                    context,
                    cancel,
                    receiver,
                    replacement,
                });
                drop(rt);
                self.project.busy.set(true);
            }
            Err(error) => {
                drop(rt);
                self.project
                    .message
                    .set(format!("Could not start search worker: {error}"));
            }
        }
    }

    fn search_project(&self) {
        if self.project.busy.get() {
            return;
        }
        let Some(context) = self.project_context() else {
            self.project
                .message
                .set("Open a folder to search its files".into());
            return;
        };
        let query = self.project.query.get();
        if query.is_empty() {
            self.project.clear_results();
            self.project.message.set("Enter text to find".into());
            return;
        }
        let options = self.project.options();
        // Cloning TextDocument is cheap. Flattening its text and walking the
        // filesystem happen on the worker, not during an input event.
        let snapshots = {
            let rt = self.runtime.lock().unwrap();
            rt.workspace
                .documents
                .iter()
                .filter_map(|doc| {
                    let path = doc.path.as_ref()?;
                    path.starts_with(&context.root).then(|| {
                        (
                            path.clone(),
                            doc.id,
                            doc.editor.revision(),
                            doc.editor.document.clone(),
                            doc.disk_text.clone(),
                        )
                    })
                })
                .collect::<Vec<_>>()
        };
        self.project.clear_results();
        self.project.message.set("Searching…".into());
        let root = context.root.clone();
        self.start_project_job(context, None, move |cancel| {
            let mut index = navigation::scan(&root, cancel)?;
            let mut open = BTreeMap::new();
            let mut excluded = BTreeSet::new();
            let mut bytes = 0usize;
            for (path, id, revision, document, disk_text) in snapshots {
                if cancel.load(Ordering::Relaxed) {
                    break;
                }
                if document.len() > project_search::MAX_FILE_BYTES {
                    excluded.insert(path);
                    continue;
                }
                if bytes.saturating_add(document.len()) > project_search::MAX_TOTAL_BYTES {
                    index.truncated = true;
                    excluded.insert(path);
                    continue;
                }
                bytes += document.len();
                let source = Source {
                    path: path.clone(),
                    text: Arc::from(document.as_str().as_ref()),
                    disk_text,
                    document: Some((id, revision)),
                };
                open.insert(path, source);
            }
            // Never silently search an old on-disk version when its open,
            // unsaved document exceeds the search budget.
            index.files.retain(|path| !excluded.contains(path));
            let mut results = project_search::scan(&index.files, open, query, options, cancel)?;
            results.skipped += excluded.len();
            results.truncated |= index.truncated;
            results.cancelled |= index.report.cancelled;
            let note = if index.truncated {
                " · folder scan incomplete".into()
            } else if index.report.unsupported_ignore_patterns > 0 {
                " · some ignore patterns unsupported".into()
            } else {
                String::new()
            };
            Ok(Output::Search(Arc::new(results), note))
        });
    }

    fn replace_project(&self) {
        if self.project.busy.get() {
            return;
        }
        if self.blocked() || self.runtime.lock().unwrap().job.is_some() {
            self.project
                .message
                .set("Finish the file operation before replacing".into());
            return;
        }
        let found = {
            let rt = self.project.runtime.lock().unwrap();
            rt.found
                .as_ref()
                .map(|found| (found.context.clone(), found.results.clone()))
        };
        let Some((context, results)) = found else {
            return;
        };
        if !self.project_context_current(&context) {
            self.project
                .message
                .set("Search again before replacing".into());
            return;
        }
        if results.hits.is_empty() {
            return;
        }
        let replacement = self
            .project
            .replacement
            .get()
            .replace("\r\n", "\n")
            .replace('\r', "\n");
        self.project.message.set("Preparing replacements…".into());
        self.start_project_job(
            context,
            Some(self.project.replacement.get()),
            move |cancel| {
                let changes = project_search::prepare_replace(&results, &replacement, cancel)?;
                Ok(Output::Replace(changes, results))
            },
        );
    }

    pub(super) fn poll_project_search(&self) {
        {
            let rt = self.project.runtime.lock().unwrap();
            if let Some(job) = &rt.job {
                if !self.project_context_current(&job.context)
                    || job
                        .replacement
                        .as_ref()
                        .is_some_and(|text| *text != self.project.replacement.get())
                {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            }
        }
        let ready = {
            let mut rt = self.project.runtime.lock().unwrap();
            let ready = rt
                .job
                .as_ref()
                .and_then(|job| match job.receiver.try_recv() {
                    Ok(result) => Some(result),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(Err("Search worker stopped before completion".into()))
                    }
                });
            ready.map(|result| (rt.job.take().unwrap(), result))
        };
        if let Some((job, output)) = ready {
            self.project.busy.set(false);
            if job.cancel.load(Ordering::Relaxed) {
                self.project
                    .message
                    .set("Cancelled · documents unchanged".into());
            } else if !self.project_context_current(&job.context) {
                self.project.clear_results();
                self.project
                    .message
                    .set("Search changed; run it again".into());
            } else {
                match output {
                    Ok(Output::Search(results, index_note)) => {
                        let rows = results
                            .hits
                            .iter()
                            .enumerate()
                            .filter_map(|(hit, result)| {
                                let source = results.sources.get(result.source)?;
                                let path = source
                                    .path
                                    .strip_prefix(&job.context.root)
                                    .unwrap_or(&source.path);
                                Some(ResultRow {
                                    key: format!(
                                        "{}:{}:{}",
                                        source.path.display(),
                                        result.range.start,
                                        hit
                                    ),
                                    hit,
                                    location: format!(
                                        "{}:{}:{}",
                                        path.display(),
                                        result.line,
                                        result.column
                                    ),
                                    preview: result.preview.replace('\t', "  "),
                                })
                            })
                            .collect();
                        self.project
                            .message
                            .set(format!("{}{}", results.summary(), index_note));
                        self.project.runtime.lock().unwrap().found = Some(Found {
                            context: job.context,
                            results,
                        });
                        self.project.rows.set(rows);
                    }
                    Ok(Output::Replace(changes, results)) => {
                        match self.apply_project_replacements(&results, changes) {
                            Ok((files, matches)) => {
                                self.project.clear_results();
                                // The triggering button is now disabled. Hand
                                // keyboard focus back for review, Undo and Save All.
                                self.project.blur();
                                let message = format!(
                                    "Replaced {matches} matches in {files} files · review and save · Undo in each tab"
                                );
                                self.project.message.set(message.clone());
                                self.status.set(message);
                            }
                            Err(error) => self.project.message.set(error),
                        }
                    }
                    Err(error) => self.project.message.set(error),
                }
            }
        }
        let stale = {
            let rt = self.project.runtime.lock().unwrap();
            rt.found
                .as_ref()
                .is_some_and(|found| !self.project_context_current(&found.context))
        };
        if stale {
            self.project.clear_results();
            self.project
                .message
                .set("Search changed; press Enter to search again".into());
        }
        self.poll_project_navigation();
    }

    fn apply_project_replacements(
        &self,
        results: &Results,
        changes: Vec<Replacement>,
    ) -> Result<(usize, usize), String> {
        if changes.len() > project_search::MAX_REPLACE_FILES
            || results.sources.len() > project_search::MAX_REPLACE_FILES
        {
            return Err("Narrow the search to at most 128 files before replacing".into());
        }
        // Obtain view-specific selections before locking the workspace. Split
        // panes have independent selections, while edits/undo share documents.
        let active = self.active_id();
        let selections = results
            .sources
            .iter()
            .filter_map(|source| {
                let (id, _) = source.document?;
                self.bindings(id)
                    .map(|bindings| (id, bindings.selection.get()))
            })
            .collect::<BTreeMap<_, _>>();
        let (ids, matches) = {
            let mut rt = self.runtime.lock().unwrap();
            if rt.dialog.is_some() || rt.close.is_some() || rt.job.is_some() {
                return Err("File operation started; search again before replacing".into());
            }
            // This is a single preflight under the same lock as the apply.
            // A stale last file must not leave the first files half-replaced.
            for source in &results.sources {
                match source.document {
                    Some((id, revision)) => {
                        if !rt.workspace.get(id).is_some_and(|doc| {
                            doc.path.as_ref() == Some(&source.path)
                                && doc.editor.revision() == revision
                        }) {
                            return Err(format!(
                                "{} changed; search again before replacing · no documents changed",
                                source.path.display()
                            ));
                        }
                    }
                    None if rt.workspace.find_path(&source.path).is_some() => {
                        return Err(format!(
                            "{} was opened after searching; search again before replacing · no documents changed",
                            source.path.display()
                        ));
                    }
                    None => {}
                }
            }
            let mut ids = Vec::new();
            let mut matches = 0;
            for change in changes {
                if change.text.as_str() == change.source.text.as_ref() {
                    continue;
                }
                let id = if let Some((id, _)) = change.source.document {
                    id
                } else {
                    let raw = change
                        .source
                        .disk_text
                        .as_deref()
                        .unwrap_or(&change.source.text);
                    let id = rt.workspace.open(change.source.path.clone(), raw);
                    rt.workspace.get_mut(id).unwrap().disk_text = change.source.disk_text.clone();
                    id
                };
                let doc = rt.workspace.get_mut(id).unwrap();
                if let Some(selection) = selections.get(&id) {
                    doc.editor.moved(*selection);
                }
                doc.editor
                    .edit_parts(&change.parts, TextSelection::collapsed(change.text.len()));
                matches += change.matches;
                ids.push(id);
            }
            (ids, matches)
        };
        for id in &ids {
            // Activating also registers documents opened from snapshots in the
            // focused editor group's tabs; restore the user's active tab below.
            self.activate(*id);
            self.sync_editor(*id);
        }
        if !ids.is_empty() {
            self.activate(active);
            self.changed();
        }
        Ok((ids.len(), matches))
    }

    fn activate_project_hit(&self, index: usize) {
        if self.blocked() {
            return;
        }
        let target = {
            let rt = self.project.runtime.lock().unwrap();
            rt.found.as_ref().and_then(|found| {
                let hit = found.results.hits.get(index)?;
                Some((
                    found.context.clone(),
                    found.results.sources.get(hit.source)?.clone(),
                    hit.range.clone(),
                ))
            })
        };
        let Some((context, source, range)) = target else {
            return;
        };
        if !self.project_context_current(&context) {
            return;
        }
        self.project.selected.set(Some(index));
        let existing = self
            .runtime
            .lock()
            .unwrap()
            .workspace
            .find_path(&source.path);
        if let Some(id) = existing {
            self.select_project_result(id, &source, range);
        } else {
            if source.document.is_some() {
                self.project
                    .message
                    .set("The searched document was closed; search again".into());
                return;
            }
            if self.runtime.lock().unwrap().job.is_some() {
                self.project
                    .message
                    .set("Finish the file operation before opening a result".into());
                return;
            }
            self.open(source.path.clone());
            let navigation = self.navigation.load(Ordering::Relaxed);
            self.project.runtime.lock().unwrap().navigation = Some(PendingNavigation {
                context,
                source,
                range,
                navigation,
            });
        }
    }

    fn select_project_result(&self, id: DocumentId, source: &Source, range: Range<usize>) {
        let current = {
            let rt = self.runtime.lock().unwrap();
            rt.workspace
                .get(id)
                .is_some_and(|doc| match source.document {
                    Some((expected, revision)) => {
                        doc.id == expected && doc.editor.revision() == revision
                    }
                    None => doc.editor.document.as_str().as_ref() == source.text.as_ref(),
                })
        };
        if !current {
            self.project
                .message
                .set("Result changed; search again to locate it".into());
            return;
        }
        self.activate(id);
        let Some(bindings) = self.bindings(id) else {
            return;
        };
        let selection = TextSelection {
            anchor: TextPosition::new(range.start),
            caret: TextPosition::new(range.end),
        };
        if let Some(doc) = self.runtime.lock().unwrap().workspace.get_mut(id) {
            doc.editor.moved(selection);
        }
        bindings.selection.set(selection);
        self.project.owns_focus.set(false);
        self.project.focus_query.set(false);
        self.terminal.blur();
        self.dismiss_language_panel();
        self.changed();
    }

    fn poll_project_navigation(&self) {
        let pending = self.project.runtime.lock().unwrap().navigation.take();
        let Some(pending) = pending else {
            return;
        };
        if self.blocked() || !self.project_context_current(&pending.context) {
            return;
        }
        let navigation = self.navigation.load(Ordering::Relaxed);
        let (id, opening) = {
            let rt = self.runtime.lock().unwrap();
            (
                rt.workspace.find_path(&pending.source.path),
                rt.job.is_some(),
            )
        };
        if let Some(id) = id {
            // File completion activates its tab once; any intervening tab/group
            // navigation contributes another generation and cancels this jump.
            if navigation == pending.navigation
                || (navigation == pending.navigation.wrapping_add(1) && self.active_id() == id)
            {
                self.select_project_result(id, &pending.source, pending.range);
            }
        } else if opening && navigation == pending.navigation {
            self.project.runtime.lock().unwrap().navigation = Some(pending);
        }
    }

    pub(super) fn project_search_view(&self) -> impl View + Clone + use<> {
        let colors = self.theme.get().colors();
        let submit = self.clone();
        let run = self.clone();
        let replace = self.clone();
        let cancel = self.project.clone();
        let close = self.project.clone();
        let open_folder = self.clone();
        let rows = self.clone();
        let focus = self.project.clone();
        let replacement_focus = self.project.clone();
        let busy = self.project.busy.get();
        let can_replace = !busy
            && self
                .project
                .runtime
                .lock()
                .unwrap()
                .found
                .as_ref()
                .is_some_and(|found| {
                    !found.results.hits.is_empty()
                        && !found.results.cancelled
                        && !found.results.truncated
                        && found.results.sources.len() <= project_search::MAX_REPLACE_FILES
                        && self.project_context_current(&found.context)
                });
        let option = |label: &'static str, state: State<bool>| {
            let selected = state.get();
            Button::new(label)
                .header_style()
                .font_size(12.0)
                .padding(5.0)
                .text_color(colors.foreground)
                .background_color(if selected {
                    colors.selection
                } else {
                    colors.surface
                })
                .on_click(move || state.update(|value| *value = !*value))
        };
        vstack! {
            hstack! {
                Text::new("SEARCH").font_size(11.0).color(colors.muted),
                Spacer::new(),
                Button::new("×").header_style().font_size(13.0).padding(4.0)
                    .text_color(colors.foreground).background_color(colors.surface).on_click(move || close.close()),
            }.spacing(4.0),
            TextField::new(self.project.query.clone()).font_size(13.0).padding(5.0)
                .background_color(colors.elevated).text_color(colors.foreground).placeholder_color(colors.muted).border_color(colors.border)
                .placeholder("Search in files").autofocus(self.project.focus_query.get()).on_submit(move || submit.search_project()).frame(f32::INFINITY, 28.0),
            hstack! {
                option("Aa", self.project.match_case.clone()),
                option("Word", self.project.whole_word.clone()),
                option(".*", self.project.regex.clone()),
                Spacer::new(),
                Button::new(if busy { "Cancel" } else { "Search" }).header_style().font_size(12.0).padding(5.0)
                    .text_color(colors.foreground).background_color(colors.surface)
                    .on_click(move || if busy { cancel.cancel() } else { run.search_project() }),
            }.spacing(3.0),
            TextField::new(self.project.replacement.clone()).font_size(13.0).padding(5.0)
                .background_color(colors.elevated).text_color(colors.foreground).placeholder_color(colors.muted).border_color(colors.border)
                .placeholder("Replace with").frame(f32::INFINITY, 28.0).on_event_capture(move |event| {
                    if matches!(event, Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })) {
                        replacement_focus.focus_query.set(false);
                    }
                    false
                }),
            Button::new("Replace All in Files").header_style().font_size(12.0).padding(5.0)
                .text_color(colors.foreground).background_color(colors.surface).enabled(can_replace)
                .on_click(move || replace.replace_project()).frame(f32::INFINITY, 28.0),
            Text::from_state(self.project.message.clone()).font_size(11.0).color(colors.muted),
            if_view!(self.runtime.lock().unwrap().explorer.root.is_none(),
                Button::new("Open Folder…").header_style().font_size(13.0).text_color(colors.foreground)
                    .background_color(colors.surface).on_click(move || open_folder.show_folder()),
                VStack::new(())),
            ListView::new(self.project.rows.clone(), self.project.selected.clone(), 48.0, move |index, row: ResultRow, selected| {
                let click = rows.clone();
                let background = if selected == Some(index) { colors.selection } else { colors.surface };
                vstack! {
                    Text::new(row.location.clone()).font_size(11.0).color(colors.muted),
                    Text::new(row.preview.clone()).font_size(13.0).color(colors.foreground),
                }.alignment(Alignment::Leading).spacing(3.0).padding(5.0)
                    .frame(f32::INFINITY,48.0).background(background)
                    .on_event(move |event| {
                        if matches!(event, Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { button: scarlet_ui::event::MouseButton::Left, .. })) {
                            click.activate_project_hit(row.hit);
                            return true;
                        }
                        false
                    })
            }).item_key(|row| row.key.clone().into()).frame(f32::INFINITY, f32::INFINITY),
        }.alignment(Alignment::Leading).spacing(6.0).padding(8.0)
            .background(colors.surface).frame(f32::INFINITY, f32::INFINITY)
            .on_event_capture(move |event| {
                if matches!(event, Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })) {
                    focus.owns_focus.set(true);
                }
                if matches!(event, Event::Keyboard(KeyEvent::Pressed { keycode: KeyCode::Escape, .. })) {
                    focus.close();
                    return true;
                }
                false
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(app: &App, name: &str, text: &str) -> DocumentId {
        let id = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .open(PathBuf::from(name), text);
        app.activate(id);
        id
    }

    fn results(app: &App, ids: &[DocumentId]) -> Results {
        let rt = app.runtime.lock().unwrap();
        let open = ids
            .iter()
            .map(|id| {
                let doc = rt.workspace.get(*id).unwrap();
                let path = doc.path.clone().unwrap();
                (
                    path.clone(),
                    Source {
                        path,
                        text: Arc::from(doc.editor.document.as_str().as_ref()),
                        disk_text: doc.disk_text.clone(),
                        document: Some((*id, doc.editor.revision())),
                    },
                )
            })
            .collect();
        project_search::scan(
            &[],
            open,
            "cat".into(),
            Options::default(),
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn text(app: &App, id: DocumentId) -> String {
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .editor
            .document
            .as_str()
            .into_owned()
    }

    #[test]
    fn replacement_preflights_every_revision_before_any_edit() {
        let app = App::new();
        let first = open(&app, "/project/a.rs", "cat one");
        let second = open(&app, "/project/b.rs", "cat two");
        let found = results(&app, &[first, second]);
        let changes =
            project_search::prepare_replace(&found, "dog", &AtomicBool::new(false)).unwrap();
        app.edit(
            second,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "new ".into(),
            },
        );
        assert!(
            app.apply_project_replacements(&found, changes)
                .unwrap_err()
                .contains("no documents changed")
        );
        assert_eq!(text(&app, first), "cat one");
        assert_eq!(text(&app, second), "new cat two");
    }

    #[test]
    fn replacement_respects_unsaved_text_and_separate_document_undo() {
        let app = App::new();
        let first = open(&app, "/project/a.rs", "cat one");
        let second = open(&app, "/project/b.rs", "cat two");
        app.edit(
            first,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "unsaved ".into(),
            },
        );
        let found = results(&app, &[first, second]);
        let changes =
            project_search::prepare_replace(&found, "dog", &AtomicBool::new(false)).unwrap();
        assert_eq!(
            app.apply_project_replacements(&found, changes).unwrap(),
            (2, 2)
        );
        assert_eq!(text(&app, first), "unsaved dog one");
        assert_eq!(text(&app, second), "dog two");
        app.activate(first);
        app.undo(false);
        assert_eq!(text(&app, first), "unsaved cat one");
        assert!(
            app.runtime
                .lock()
                .unwrap()
                .workspace
                .get(first)
                .unwrap()
                .editor
                .dirty()
        );
        assert_eq!(text(&app, second), "dog two");
        app.activate(second);
        app.undo(false);
        assert_eq!(text(&app, second), "cat two");
        assert!(
            !app.runtime
                .lock()
                .unwrap()
                .workspace
                .get(second)
                .unwrap()
                .editor
                .dirty()
        );
    }

    #[test]
    fn unopened_replacement_preserves_raw_baseline_and_registers_tab() {
        let app = App::new();
        let previous = app.active_id();
        let source = Source {
            path: PathBuf::from("/project/closed.rs"),
            text: "cat\n".into(),
            disk_text: Some("cat\r\n".into()),
            document: None,
        };
        let found = Results {
            sources: vec![source.clone()],
            hits: Vec::new(),
            skipped: 0,
            truncated: false,
            cancelled: false,
            query: "cat".into(),
            options: Options::default(),
        };
        let changes = vec![Replacement {
            parts: vec![EditDelta {
                replaced_range: 0..3,
                inserted: "dog".into(),
            }],
            source,
            text: "dog\n".into(),
            matches: 1,
        }];
        assert_eq!(
            app.apply_project_replacements(&found, changes).unwrap(),
            (1, 1)
        );
        assert_eq!(app.active_id(), previous);
        let id = {
            let rt = app.runtime.lock().unwrap();
            let id = rt
                .workspace
                .find_path(Path::new("/project/closed.rs"))
                .unwrap();
            let doc = rt.workspace.get(id).unwrap();
            assert_eq!(doc.disk_text.as_deref(), Some("cat\r\n"));
            assert!(doc.crlf && doc.editor.dirty());
            assert!(rt.group_tabs[app.focused_group.get()].contains(&id));
            id
        };
        app.activate(id);
        app.undo(false);
        assert_eq!(text(&app, id), "cat\n");
        assert!(
            !app.runtime
                .lock()
                .unwrap()
                .workspace
                .get(id)
                .unwrap()
                .editor
                .dirty()
        );
    }

    #[test]
    fn opening_result_after_scan_aborts_all_replacement() {
        let app = App::new();
        let first = open(&app, "/project/a.rs", "cat");
        let mut found = results(&app, &[first]);
        let closed = Source {
            path: PathBuf::from("/project/b.rs"),
            text: "cat".into(),
            disk_text: Some("cat".into()),
            document: None,
        };
        found.sources.push(closed.clone());
        let changes = vec![
            Replacement {
                parts: vec![EditDelta {
                    replaced_range: 0..3,
                    inserted: "dog".into(),
                }],
                source: found.sources[0].clone(),
                text: "dog".into(),
                matches: 1,
            },
            Replacement {
                parts: vec![EditDelta {
                    replaced_range: 0..3,
                    inserted: "dog".into(),
                }],
                source: closed,
                text: "dog".into(),
                matches: 1,
            },
        ];
        open(&app, "/project/b.rs", "unsaved cat");
        assert!(app.apply_project_replacements(&found, changes).is_err());
        assert_eq!(text(&app, first), "cat");
    }

    fn queued_search(app: &App, context: Context, found: Results) {
        let (sender, receiver) = mpsc::channel();
        sender
            .send(Ok(Output::Search(Arc::new(found), String::new())))
            .unwrap();
        app.project.runtime.lock().unwrap().job = Some(Job {
            context,
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
            replacement: None,
        });
        app.project.busy.set(true);
    }

    #[test]
    fn completed_search_with_new_query_or_root_is_discarded() {
        let app = App::new();
        app.runtime.lock().unwrap().explorer.root = Some(PathBuf::from("/project"));
        let id = open(&app, "/project/a.rs", "cat");
        app.project.query.set("cat".into());
        queued_search(&app, app.project_context().unwrap(), results(&app, &[id]));
        app.project.query.set("dog".into());
        app.poll_project_search();
        assert!(!app.project.busy.get());
        assert!(app.project.rows.get().is_empty());
        assert!(app.project.runtime.lock().unwrap().found.is_none());

        queued_search(&app, app.project_context().unwrap(), results(&app, &[id]));
        app.runtime.lock().unwrap().explorer.generation += 1;
        app.poll_project_search();
        assert!(app.project.runtime.lock().unwrap().found.is_none());
    }

    #[test]
    fn replacement_input_changed_while_worker_ran_does_not_apply() {
        let app = App::new();
        app.runtime.lock().unwrap().explorer.root = Some(PathBuf::from("/project"));
        let id = open(&app, "/project/a.rs", "cat");
        let found = Arc::new(results(&app, &[id]));
        let changes =
            project_search::prepare_replace(&found, "dog", &AtomicBool::new(false)).unwrap();
        let (sender, receiver) = mpsc::channel();
        sender.send(Ok(Output::Replace(changes, found))).unwrap();
        app.project.runtime.lock().unwrap().job = Some(Job {
            context: app.project_context().unwrap(),
            cancel: Arc::new(AtomicBool::new(false)),
            receiver,
            replacement: Some("dog".into()),
        });
        app.project.replacement.set("bird".into());
        app.poll_project_search();
        assert_eq!(text(&app, id), "cat");
    }

    #[test]
    fn stale_result_activation_preserves_selection() {
        let app = App::new();
        let id = open(&app, "/project/a.rs", "cat one");
        let found = results(&app, &[id]);
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: "new ".into(),
            },
        );
        let before = app.bindings(id).unwrap().selection.get();
        app.select_project_result(id, &found.sources[0], 0..3);
        assert_eq!(app.bindings(id).unwrap().selection.get(), before);
    }

    #[test]
    fn search_result_and_replacement_use_focused_split_bindings() {
        let app = App::new();
        let id = open(&app, "/project/a.rs", "cat one\ncat two");
        let left = app.bindings(id).unwrap();
        left.selection.set(TextSelection::collapsed(5));
        app.split_editor(false);
        let right = app.bindings(id).unwrap();
        let found = results(&app, &[id]);
        app.select_project_result(id, &found.sources[0], 8..11);
        assert_eq!(left.selection.get(), TextSelection::collapsed(5));
        assert_eq!(right.selection.get().normalized_range(), Some(8..11));
        let changes =
            project_search::prepare_replace(&found, "dog", &AtomicBool::new(false)).unwrap();
        app.apply_project_replacements(&found, changes).unwrap();
        assert_eq!(left.document.get().as_str(), "dog one\ndog two");
        assert_eq!(left.selection.get(), TextSelection::collapsed(5));
        app.undo(false);
        assert_eq!(right.selection.get().normalized_range(), Some(8..11));
        assert_eq!(left.document.get().as_str(), "cat one\ncat two");
    }

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "kureha-project-ui-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir(&path).unwrap();
            Self(std::fs::canonicalize(path).unwrap())
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn finish_search(app: &App) {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.project.busy.get() {
            app.poll_project_search();
            assert!(
                std::time::Instant::now() < deadline,
                "project worker did not finish"
            );
            thread::sleep(std::time::Duration::from_millis(1));
        }
    }

    #[test]
    fn real_worker_searches_unsaved_snapshot_and_replace_never_writes_disk() {
        let dir = Fixture::new();
        let first_path = dir.0.join("a.txt");
        let second_path = dir.0.join("b.txt");
        std::fs::write(&first_path, "cat disk\r\n").unwrap();
        std::fs::write(&second_path, "cat two\r\n").unwrap();
        std::fs::create_dir(dir.0.join("target")).unwrap();
        std::fs::write(dir.0.join("target/ignored.txt"), "cat build").unwrap();
        let app = App::new();
        app.runtime.lock().unwrap().explorer.root = Some(dir.0.clone());
        let first = open(&app, first_path.to_str().unwrap(), "cat disk\r\n");
        app.edit(
            first,
            &EditDelta {
                replaced_range: 4..8,
                inserted: "unsaved".into(),
            },
        );
        app.project.query.set("cat".into());
        app.search_project();
        finish_search(&app);
        assert_eq!(app.project.rows.get().len(), 2);
        assert!(
            app.project
                .rows
                .get()
                .iter()
                .any(|row| row.preview.contains("unsaved"))
        );
        app.project.replacement.set("dog".into());
        app.replace_project();
        finish_search(&app);
        assert_eq!(text(&app, first), "dog unsaved\n");
        let second = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .find_path(&second_path)
            .unwrap();
        assert_eq!(text(&app, second), "dog two\n");
        assert_eq!(std::fs::read_to_string(first_path).unwrap(), "cat disk\r\n");
        assert_eq!(std::fs::read_to_string(second_path).unwrap(), "cat two\r\n");
        assert!(app.project.message.get().contains("review and save"));
    }

    #[test]
    fn mounted_search_enter_and_result_click_return_keyboard_to_editor() {
        use scarlet_ui::{
            event::{KeyModifiers, MouseButton, MouseEvent},
            pipeline::RenderingPipeline,
            renderer::{PaintCommand, PaintContext},
        };
        let dir = Fixture::new();
        let path = dir.0.join("a.txt");
        std::fs::write(&path, "cat result\n").unwrap();
        let app = App::new();
        let id = open(&app, path.to_str().unwrap(), "cat result\n");
        app.runtime.lock().unwrap().explorer.root = Some(dir.0.clone());
        app.show_project_search();
        app.project.query.set("cat".into());
        let window = || {
            Window::new("Project search", app.content())
                .decorated(false)
                .size(Size::new(1100.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render();
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Enter,
            modifiers: KeyModifiers::empty(),
        }));
        assert!(
            app.project.busy.get(),
            "Enter did not submit the focused search field"
        );
        finish_search(&app);
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.layout_initial();
        pipeline.render();
        fn locate(e: &dyn Element, origin: Point, label: &str) -> Option<Point> {
            let p = Point::new(origin.x + e.position().x, origin.y + e.position().y);
            if let Some(render) = e.render_object() {
                let mut paint = PaintContext::new();
                render.paint(&mut paint, Point::ZERO);
                if paint.commands().iter().any(|command| matches!(command, PaintCommand::DrawText { text, .. } if text == label)) {
                    return Some(Point::new(p.x + 5.0, p.y + 5.0));
                }
            }
            e.children()
                .iter()
                .find_map(|child| locate(child.as_ref(), p, label))
        }
        let hit = locate(
            pipeline.element_tree().root().unwrap(),
            Point::ZERO,
            "a.txt:1:1",
        )
        .expect("visible real search result");
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
            button: MouseButton::Left,
            x: hit.x as i32,
            y: hit.y as i32,
            click_count: 1,
        }));
        pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
            button: MouseButton::Left,
            x: hit.x as i32,
            y: hit.y as i32,
            click_count: 1,
        }));
        assert_eq!(
            app.bindings(id).unwrap().selection.get().normalized_range(),
            Some(0..3)
        );
        assert!(
            app.project.is_open(),
            "result activation must retain the search sidebar"
        );
        assert!(!app.project.wants_focus());
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.layout_initial();
        pipeline.render();
        let mut modifiers = KeyModifiers::empty();
        if cfg!(target_os = "macos") {
            modifiers.super_key = true;
        } else {
            modifiers.control = true;
        }
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Char('a'),
            modifiers,
        }));
        assert_eq!(
            app.bindings(id).unwrap().selection.get().normalized_range(),
            Some(0..11),
            "Select All did not return to the editor after a search result click"
        );
        assert_eq!(app.project.query.get(), "cat");
        app.show_project_search();
        app.search_project();
        finish_search(&app);
        app.project.replacement.set("dog".into());
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.layout_initial();
        pipeline.render();
        let button = locate(
            pipeline.element_tree().root().unwrap(),
            Point::ZERO,
            "Replace All in Files",
        )
        .unwrap();
        for event in [
            MouseEvent::ButtonPressed {
                button: MouseButton::Left,
                x: button.x as i32,
                y: button.y as i32,
                click_count: 1,
            },
            MouseEvent::ButtonReleased {
                button: MouseButton::Left,
                x: button.x as i32,
                y: button.y as i32,
                click_count: 1,
            },
        ] {
            pipeline.handle_event(&Event::Mouse(event));
        }
        finish_search(&app);
        assert!(
            !app.project.wants_focus(),
            "disabled Replace All retained keyboard focus"
        );
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.layout_initial();
        pipeline.render();
        modifiers.alt = true;
        pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
            keycode: KeyCode::Char('s'),
            modifiers,
        }));
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while app.runtime.lock().unwrap().job.is_some() {
            app.poll();
            assert!(std::time::Instant::now() < deadline);
            std::thread::yield_now();
        }
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "dog result\n",
            "Save All shortcut was lost after replacement"
        );
        pipeline.teardown();
    }
    #[test]
    fn unopened_search_result_navigates_after_worker_invalidates_clipboard_generation() {
        for split in [false, true] {
            let dir = Fixture::new();
            let path = dir.0.join("unopened.txt");
            std::fs::write(&path, "before\ncat result\n").unwrap();
            let app = App::new();
            if split {
                app.split_editor(false);
            }
            app.runtime.lock().unwrap().explorer.root = Some(dir.0.clone());
            app.show_project_search();
            app.project.query.set("cat".into());
            app.search_project();
            finish_search(&app);
            app.activate_project_hit(0);
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            while app.runtime.lock().unwrap().job.is_some() {
                app.poll();
                app.poll_project_search();
                assert!(std::time::Instant::now() < deadline);
                std::thread::yield_now();
            }
            app.poll_project_search();
            assert_eq!(
                app.bindings(app.active_id())
                    .unwrap()
                    .selection
                    .get()
                    .normalized_range(),
                Some(7..10)
            );
            assert!(!app.project.wants_focus());
        }
    }
}
