//! Ordinary Markdown preview tabs with shared documents and local scroll synchronization.
use super::*;
use kureha_code::{
    markdown::{self, Document},
    markdown_resources,
    theme::Theme,
};
use std::sync::atomic::AtomicBool;

#[derive(Clone, PartialEq)]
struct Key {
    revision: u64,
    path: Option<PathBuf>,
    theme: Theme,
}
struct Entry {
    key: Key,
    document: Arc<Document>,
    pending: BTreeSet<String>,
}
struct Job {
    id: DocumentId,
    key: Key,
    receiver: mpsc::Receiver<Arc<Document>>,
    cancel: Arc<AtomicBool>,
}
impl Drop for Job {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
struct ImageJob {
    id: DocumentId,
    path: Option<PathBuf>,
    destination: String,
    receiver: mpsc::Receiver<markdown_resources::Resource>,
}
#[derive(Clone)]
pub(super) struct PreviewTab {
    pub document: DocumentId,
    pub source: usize,
    pub follow_file: bool,
    pub sync_position: bool,
}
#[derive(Clone)]
pub(super) struct PreviewView {
    pub position: Arc<Mutex<crate::markdown_view::PreviewPosition>>,
    pub reveal: State<Option<(f32, f32)>>,
    pub requested_y: Option<f32>,
    last_source_y: f32,
    last_preview_y: f32,
    last_geometry: u64,
    last_document: Option<DocumentId>,
}
impl Default for PreviewView {
    fn default() -> Self {
        Self {
            position: Default::default(),
            reveal: State::new(generate_state_id(), None),
            requested_y: None,
            last_source_y: -1.0,
            last_preview_y: 0.0,
            last_geometry: 0,
            last_document: None,
        }
    }
}
impl PreviewView {
    fn reveal(&mut self, y: f32) {
        let height = self.position.lock().unwrap().viewport.size.height.max(1.0);
        self.requested_y = Some(y);
        let range = Some((y, y + height));
        if self.reveal.get() != range {
            self.reveal.set(range);
        }
    }
}
#[derive(Default)]
pub(super) struct Previews {
    pub tabs: BTreeMap<TabId, PreviewTab>,
    views: BTreeMap<(TabId, usize), PreviewView>,
    entries: BTreeMap<DocumentId, Entry>,
    job: Option<Job>,
    image_job: Option<ImageJob>,
    links: Vec<(DocumentId, String)>,
}
impl Previews {
    pub fn remove(&mut self, id: DocumentId) {
        self.entries.remove(&id);
        self.links.retain(|(document, _)| *document != id);
        self.tabs.retain(|_, tab| tab.document != id);
        self.views.retain(|(tab, _), _| self.tabs.contains_key(tab));
        if self.job.as_ref().is_some_and(|job| job.id == id) {
            self.job = None;
        }
    }
    pub fn move_view(&mut self, tab: TabId, source: usize, target: usize, copy: bool) {
        if source == target {
            return;
        }
        let view = if copy {
            self.views.get(&(tab, source)).cloned().map(|mut view| {
                let position = view.position.lock().unwrap().clone();
                view.position = Arc::new(Mutex::new(position));
                view.reveal = State::new(generate_state_id(), None);
                view
            })
        } else {
            self.views.remove(&(tab, source))
        };
        if let Some(mut view) = view {
            let y = view.position.lock().unwrap().viewport.origin.y;
            view.reveal(y);
            self.views.insert((tab, target), view);
        }
    }
    pub fn views_forget(&mut self, tab: TabId, group: usize) {
        self.views.remove(&(tab, group));
    }
    pub fn retire_pane(&mut self, group: usize) {
        self.views.retain(|(_, g), _| *g != group);
    }
    pub fn remove_tab(&mut self, tab: TabId) {
        self.tabs.remove(&tab);
        self.views.retain(|(t, _), _| *t != tab);
    }
}
impl Runtime {
    pub(super) fn document_for_tab(&self, tab: TabId) -> Option<DocumentId> {
        self.markdown
            .tabs
            .get(&tab)
            .map(|p| p.document)
            .or_else(|| self.workspace.get(tab).map(|d| d.id))
    }
}
impl App {
    pub(super) fn group_document(&self, group: usize) -> Option<DocumentId> {
        self.group_id(group)
            .and_then(|tab| self.runtime.lock().unwrap().document_for_tab(tab))
    }
    pub(super) fn preview_tab(&self, group: usize) -> Option<TabId> {
        self.group_id(group)
            .filter(|tab| self.runtime.lock().unwrap().markdown.tabs.contains_key(tab))
    }
    #[cfg(test)]
    pub(super) fn markdown_previewing(&self, id: DocumentId, group: usize) -> bool {
        self.preview_tab(group).is_some() && self.group_document(group) == Some(id)
    }
    pub(super) fn open_markdown_preview(&self) {
        self.open_preview(false);
    }
    pub(super) fn markdown_preview_to_side(&self) {
        self.open_preview(true);
    }
    fn open_preview(&self, beside: bool) {
        if self.blocked() {
            return;
        }
        if self.preview_tab(self.focused_group.get()).is_some() {
            return;
        }
        let Some(id) = self.active_id() else {
            return;
        };
        let source = self.focused_group.get();
        let existing = {
            let rt = self.runtime.lock().unwrap();
            let Some(doc) = rt.workspace.get(id) else {
                return;
            };
            if doc.image.is_some() || !doc.path.as_deref().is_none_or(markdown::is_markdown) {
                return;
            }
            rt.markdown
                .tabs
                .iter()
                .find_map(|(tab, p)| (p.document == id).then_some(*tab))
        };
        if let Some(tab) = existing {
            let group = self
                .runtime
                .lock()
                .unwrap()
                .group_tabs
                .iter()
                .position(|tabs| tabs.contains(&tab));
            if let Some(group) = group {
                if beside && group == source {
                    self.split_tab_beside(tab, source, source, tab_drag::Edge::Right);
                } else {
                    self.focus_group(group);
                    self.activate_unchecked(tab);
                }
                return;
            }
        }
        let tab = {
            let mut rt = self.runtime.lock().unwrap();
            let tab = rt.workspace.new_tab_id();
            rt.markdown.tabs.insert(
                tab,
                PreviewTab {
                    document: id,
                    source,
                    follow_file: true,
                    sync_position: true,
                },
            );
            tab
        };
        let group = if beside {
            self.add_group(tab, source, tab_drag::Edge::Right)
        } else {
            source
        };
        self.focus_group(group);
        self.activate_unchecked(tab);
        self.poll_markdown();
        self.changed();
    }
    fn sync_preview_files(&self) {
        let panes = self.panes.get();
        let order = self.pane_order();
        let mut rt = self.runtime.lock().unwrap();
        let tabs: Vec<_> = rt
            .markdown
            .tabs
            .iter()
            .map(|(t, p)| (*t, p.clone()))
            .collect();
        let mut changed = false;
        for (tab, mut preview) in tabs {
            if !order.contains(&preview.source) {
                if let Some(group) = order
                    .iter()
                    .find(|g| rt.group_tabs[**g].contains(&preview.document))
                    .or(order.first())
                {
                    preview.source = *group;
                }
            }
            if preview.follow_file {
                if let Some(id) = panes
                    .get(preview.source)
                    .copied()
                    .flatten()
                    .filter(|id| !rt.markdown.tabs.contains_key(id))
                {
                    if rt.workspace.get(id).is_some_and(|d| {
                        d.image.is_none() && d.path.as_deref().is_none_or(markdown::is_markdown)
                    }) && preview.document != id
                    {
                        preview.document = id;
                        rt.markdown.views.retain(|(t, _), _| *t != tab);
                        changed = true;
                    }
                }
            }
            rt.markdown.tabs.insert(tab, preview);
        }
        drop(rt);
        if changed {
            self.changed();
        }
    }
    fn preview_follow(&self, group: usize) {
        let Some(tab) = self.preview_tab(group) else {
            return;
        };
        if let Some(preview) = self.runtime.lock().unwrap().markdown.tabs.get_mut(&tab) {
            preview.follow_file = !preview.follow_file;
        }
        self.sync_preview_files();
        self.changed();
    }
    fn preview_scroll_sync(&self, group: usize) {
        let Some(tab) = self.preview_tab(group) else {
            return;
        };
        let mut rt = self.runtime.lock().unwrap();
        if let Some(preview) = rt.markdown.tabs.get_mut(&tab) {
            preview.sync_position = !preview.sync_position;
        }
        for ((t, _), view) in &mut rt.markdown.views {
            if *t == tab {
                view.last_document = None;
                view.requested_y = None;
            }
        }
        drop(rt);
        self.changed();
    }
    pub(super) fn markdown_follow_toggle(&self) {
        self.preview_follow(self.focused_group.get());
    }
    pub(super) fn markdown_scroll_toggle(&self) {
        self.preview_scroll_sync(self.focused_group.get());
    }
    pub(super) fn markdown_source(&self) {
        let Some(tab) = self.preview_tab(self.focused_group.get()) else {
            return;
        };
        let preview = self.runtime.lock().unwrap().markdown.tabs[&tab].clone();
        if self.group_exists(preview.source) {
            self.focus_group(preview.source);
        }
        self.activate(preview.document);
    }
    pub(super) fn refresh_markdown_preview(&self) {
        let Some(id) = self.active_id() else {
            return;
        };
        self.runtime.lock().unwrap().markdown.entries.remove(&id);
        self.poll_markdown();
        self.changed();
    }
    pub(super) fn poll_markdown(&self) {
        self.sync_preview_files();
        let visible: BTreeSet<_> = self
            .panes
            .get()
            .iter()
            .enumerate()
            .filter_map(|(group, id)| id.map(|id| (id, group)))
            .collect();
        let theme = self.theme.get();
        let mut rt = self.runtime.lock().unwrap();
        let wanted: BTreeMap<_, _> = visible
            .iter()
            .filter_map(|(tab, _)| {
                let preview = rt.markdown.tabs.get(tab)?;
                let doc = rt.workspace.get(preview.document)?;
                Some((
                    doc.id,
                    Key {
                        revision: doc.editor.revision(),
                        path: doc.path.clone(),
                        theme: theme.clone(),
                    },
                ))
            })
            .collect();
        let mut changed = false;
        if let Some(job) = rt.markdown.job.as_ref() {
            if wanted.get(&job.id) != Some(&job.key) {
                job.cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Some(job) = rt.markdown.job.as_ref() {
            match job.receiver.try_recv() {
                Ok(document) => {
                    if wanted.get(&job.id) == Some(&job.key) {
                        let id = job.id;
                        let key = job.key.clone();
                        let pending = document
                            .blocks
                            .iter()
                            .flat_map(|b| b.cells.iter().flatten())
                            .filter_map(|s| s.style.image.clone())
                            .collect();
                        rt.markdown.entries.insert(
                            id,
                            Entry {
                                key,
                                document,
                                pending,
                            },
                        );
                        changed = true;
                    }
                    rt.markdown.job = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    rt.markdown.job = None;
                }
            }
        }
        // Image I/O has its own single worker: a slow URL never delays text revisions.
        let image_result =
            rt.markdown
                .image_job
                .as_ref()
                .and_then(|job| match job.receiver.try_recv() {
                    Ok(resource) => Some(resource),
                    Err(mpsc::TryRecvError::Empty) => None,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Some(markdown_resources::Resource::error("Image worker stopped"))
                    }
                });
        if let Some(resource) = image_result {
            let job = rt.markdown.image_job.take().unwrap();
            if let Some(entry) = rt.markdown.entries.get_mut(&job.id).filter(|entry| {
                entry.key.path == job.path && entry.pending.contains(&job.destination)
            }) {
                let pixels: usize = entry
                    .document
                    .images
                    .iter()
                    .filter(|(url, _)| **url != job.destination)
                    .filter_map(|(_, r)| r.result.as_ref().ok())
                    .map(|b| b.width() as usize * b.height() as usize)
                    .sum();
                let count = resource
                    .result
                    .as_ref()
                    .map_or(0, |b| b.width() as usize * b.height() as usize);
                let resource = if pixels + count > markdown_resources::MAX_DOCUMENT_PIXELS {
                    markdown_resources::Resource::error("Preview image budget exceeded")
                } else {
                    resource
                };
                Arc::make_mut(&mut entry.document)
                    .images
                    .insert(job.destination.clone(), resource);
                entry.pending.remove(&job.destination);
                changed = true;
            }
        }
        if rt.markdown.job.is_none() {
            if let Some((&id, key)) = wanted.iter().find(|(id, key)| {
                rt.markdown
                    .entries
                    .get(id)
                    .is_none_or(|entry| entry.key != **key)
            }) {
                let snapshot = rt.workspace.get(id).unwrap().editor.document.clone();
                let key = key.clone();
                let request = key.clone();
                let previous = rt
                    .markdown
                    .entries
                    .get(&id)
                    .filter(|e| e.key.path == key.path)
                    .map(|entry| entry.document.clone());
                let (sender, receiver) = mpsc::sync_channel(1);
                let cancel = Arc::new(AtomicBool::new(false));
                let cancelled = cancel.clone();
                let spawned = thread::Builder::new()
                    .name("markdown-preview".into())
                    .spawn(move || {
                        let mut document = markdown::parse(&snapshot.as_str(), &request.theme);
                        let destinations: BTreeSet<_> = document
                            .blocks
                            .iter()
                            .flat_map(|b| b.cells.iter().flatten())
                            .filter_map(|s| s.style.image.clone())
                            .collect();
                        if let Some(previous) = &previous {
                            for destination in &destinations {
                                if let Some(resource) = previous.images.get(destination) {
                                    document
                                        .images
                                        .insert(destination.clone(), resource.clone());
                                }
                            }
                        }
                        if !cancelled.load(Ordering::Relaxed) {
                            let _ = sender.send(Arc::new(document));
                        }
                    });
                if spawned.is_ok() {
                    rt.markdown.job = Some(Job {
                        id,
                        key,
                        receiver,
                        cancel,
                    });
                }
            }
        }
        if rt.markdown.image_job.is_none() {
            if let Some((&id, entry)) = rt
                .markdown
                .entries
                .iter_mut()
                .find(|(id, entry)| wanted.contains_key(id) && !entry.pending.is_empty())
            {
                // Keep an explicit error for excess images instead of silently dropping content.
                let destinations: Vec<_> = entry
                    .document
                    .blocks
                    .iter()
                    .flat_map(|b| b.cells.iter().flatten())
                    .filter_map(|s| s.style.image.clone())
                    .collect::<BTreeSet<_>>()
                    .into_iter()
                    .collect();
                for destination in destinations.into_iter().skip(32) {
                    entry.pending.remove(&destination);
                    Arc::make_mut(&mut entry.document).images.insert(
                        destination,
                        markdown_resources::Resource::error("Preview is limited to 32 images"),
                    );
                    changed = true;
                }
                if let Some(destination) = entry.pending.first().cloned() {
                    let path = entry.key.path.clone();
                    let previous = entry.document.images.get(&destination).cloned();
                    let request_path = path.clone();
                    let request_url = destination.clone();
                    let (sender, receiver) = mpsc::sync_channel(1);
                    let spawned =
                        thread::Builder::new()
                            .name("markdown-image".into())
                            .spawn(move || {
                                let resource = markdown_resources::load(
                                    &request_url,
                                    request_path.as_deref(),
                                    previous.as_ref(),
                                );
                                let _ = sender.send(resource);
                            });
                    if spawned.is_ok() {
                        rt.markdown.image_job = Some(ImageJob {
                            id,
                            path,
                            destination,
                            receiver,
                        });
                    }
                }
            }
        }
        // Retain only a few hidden documents; visible previews keep their shared assets.
        while rt.markdown.entries.len() > wanted.len().max(4) {
            let Some(id) = rt
                .markdown
                .entries
                .keys()
                .find(|id| !wanted.contains_key(id))
                .copied()
            else {
                break;
            };
            rt.markdown.entries.remove(&id);
        }
        let links = std::mem::take(&mut rt.markdown.links);
        drop(rt);
        if changed {
            self.markdown_revision
                .update(|revision| *revision = revision.wrapping_add(1));
        }
        for (id, link) in links {
            self.open_markdown_link(id, &link);
        }
        self.sync_preview_position();
    }
    fn preview_source_line_height(&self) -> f32 {
        let font_size = self.theme.get().editor_font_size();
        self.document_font()
            .as_ref()
            .map_or_else(
                || scarlet_ui::graphics::line_height_sized(font_size),
                |font| scarlet_ui::graphics::line_height_sized_with_font_stack(font_size, font),
            )
            .max(1) as f32
    }
    fn request_preview_scroll(&self, group: usize, y: f32) {
        let Some(tab) = self.preview_tab(group) else {
            return;
        };
        let source = {
            let mut rt = self.runtime.lock().unwrap();
            let preview = rt.markdown.tabs[&tab].clone();
            let view = rt.markdown.views.entry((tab, group)).or_default();
            let position = view.position.lock().unwrap();
            let max = (position.mapping.last().map_or(0.0, |p| p.1)
                - position.viewport.size.height)
                .max(0.0);
            let y = y.clamp(0.0, max);
            let source = preview
                .sync_position
                .then(|| (preview.source, preview.document, position.source_line(y)));
            drop(position);
            view.reveal(y);
            source
        };
        if let Some((source, id, line)) = source {
            if self.group_id(source) == Some(id) {
                if let Some(bindings) = self.bindings_for(id, source) {
                    let scroll = bindings.scroll.get();
                    bindings.scroll.set(TextViewScroll {
                        x: scroll.x,
                        y: line * self.preview_source_line_height(),
                    });
                }
            }
        }
    }
    fn sync_preview_position(&self) {
        let active = self.panes.get();
        let previews: Vec<_> = {
            let rt = self.runtime.lock().unwrap();
            active
                .iter()
                .enumerate()
                .filter_map(|(group, tab)| {
                    let tab = (*tab)?;
                    Some((group, tab, rt.markdown.tabs.get(&tab)?.clone()))
                })
                .collect()
        };
        if previews.is_empty() {
            return;
        }
        let line_height = self.preview_source_line_height();
        for (group, tab, preview) in previews {
            let bindings = (self.group_id(preview.source) == Some(preview.document))
                .then(|| self.bindings_for(preview.document, preview.source))
                .flatten();
            let source_scroll = bindings.as_ref().map(|b| b.scroll.get());
            let mut reverse = None;
            {
                let mut rt = self.runtime.lock().unwrap();
                let model = rt
                    .markdown
                    .entries
                    .get(&preview.document)
                    .filter(|e| {
                        rt.workspace
                            .get(preview.document)
                            .is_some_and(|d| d.editor.revision() == e.key.revision)
                    })
                    .map(|e| Arc::as_ptr(&e.document) as usize);
                let Some(view) = rt.markdown.views.get_mut(&(tab, group)) else {
                    continue;
                };
                let position = view.position.lock().unwrap();
                if position.viewport.size.height <= 0.0 || Some(position.model) != model {
                    continue;
                }
                let y = position.viewport.origin.y;
                let max = (position.mapping.last().map_or(0.0, |p| p.1)
                    - position.viewport.size.height)
                    .max(0.0);
                if let Some(request) = view.requested_y {
                    let target = request.clamp(0.0, max);
                    if (y - target).abs() < 1.0 {
                        view.requested_y = None;
                        view.last_preview_y = y;
                        view.last_geometry = position.geometry;
                    } else if target != request {
                        drop(position);
                        view.reveal(target);
                    }
                    continue;
                }
                let Some(source_scroll) = source_scroll else {
                    continue;
                };
                let mut request = None;
                if preview.sync_position
                    && (view.last_document != Some(preview.document)
                        || (source_scroll.y - view.last_source_y).abs() > 0.5
                        || view.last_geometry != position.geometry
                            && (y - view.last_preview_y).abs() <= 0.5)
                {
                    let target = position
                        .preview_y(source_scroll.y / line_height)
                        .clamp(0.0, max);
                    if (y - target).abs() > 1.0 {
                        request = Some(target);
                    }
                } else if preview.sync_position && (y - view.last_preview_y).abs() > 0.5 {
                    reverse = Some(position.source_line(y) * line_height);
                }
                view.last_source_y = reverse.unwrap_or(source_scroll.y);
                view.last_preview_y = y;
                view.last_geometry = position.geometry;
                view.last_document = Some(preview.document);
                drop(position);
                if let Some(y) = request {
                    view.reveal(y);
                }
            }
            if let (Some(y), Some(bindings)) = (reverse, bindings) {
                let scroll = bindings.scroll.get();
                bindings.scroll.set(TextViewScroll { x: scroll.x, y });
            }
        }
    }
    fn open_markdown_link(&self, id: DocumentId, link: &str) {
        if link.starts_with("https://")
            || link.starts_with("http://")
            || link.starts_with("mailto:")
        {
            #[cfg(target_os = "macos")]
            let result = std::process::Command::new("open").arg(link).spawn();
            #[cfg(target_os = "windows")]
            let result = std::process::Command::new("explorer.exe").arg(link).spawn();
            #[cfg(all(not(target_os = "macos"), not(target_os = "windows")))]
            let result = std::process::Command::new("xdg-open").arg(link).spawn();
            if let Err(error) = result {
                self.status.set(format!("Could not open link: {error}"));
            }
            return;
        }
        let path = self
            .runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .and_then(|doc| doc.path.clone());
        if link.starts_with('#') {
            self.status.set("Heading not found".into());
            return;
        }
        match markdown_resources::local_path(link, path.as_deref()) {
            Ok(path) => self.open(path),
            Err(error) => self.status.set(error),
        }
    }
    pub(super) fn markdown_view(&self, tab: TabId, group: usize) -> impl View + Clone + use<> {
        LivePreview {
            app: self.clone(),
            tab,
            group,
        }
    }
    fn markdown_content(&self, tab: TabId, group: usize) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let (id, view, document) = {
            let mut rt = self.runtime.lock().unwrap();
            let id = rt.markdown.tabs[&tab].document;
            let view = rt.markdown.views.entry((tab, group)).or_default().clone();
            let document = rt
                .markdown
                .entries
                .get(&id)
                .map(|e| e.document.clone())
                .unwrap_or_default();
            (id, view, document)
        };
        let app = self.clone();
        let focus = self.clone();
        let scroll = self.clone();
        let body = ScrollView::new(crate::markdown_view::MarkdownView {
            document,
            theme: theme.clone(),
            font: self.document_font(),
            position: view.position,
            on_scroll: Arc::new(move |y| scroll.request_preview_scroll(group, y)),
            on_link: Arc::new(move |link| {
                app.runtime.lock().unwrap().markdown.links.push((id, link))
            }),
        })
        .scrollbar_color(theme.colors().muted)
        .scroll_to_vertical_range_state(view.reveal);
        Surface::section(
            body.frame(f32::INFINITY, f32::INFINITY)
                .clip()
                .key(format!("markdown-{tab}-{id}")),
        )
        .fill(theme.editor_colors().background)
        .corner_radius(12.0)
        .clip_content(true)
        .padding_insets(EdgeInsets::new(4.0, 0.0, 4.0, 4.0))
        .on_event_capture(move |event| {
            if matches!(
                event,
                Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })
            ) {
                focus.focus_group(group);
            }
            false
        })
    }
}

#[derive(Clone)]
struct LivePreview {
    app: App,
    tab: TabId,
    group: usize,
}
impl View for LivePreview {
    fn create_element(&self) -> Box<dyn Element> {
        Box::new(scarlet_ui::element::ComponentElement::new_with_builder(
            self.clone(),
            |view| Box::new(view.app.markdown_content(view.tab, view.group)),
        ))
    }
    fn listenables(&self) -> Vec<&dyn Listenable> {
        vec![&self.app.markdown_revision]
    }
    fn as_any(&self) -> &dyn std::any::Any {
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    fn preview_state(app: &App, group: usize) -> PreviewView {
        let tab = app.preview_tab(group).unwrap();
        app.runtime
            .lock()
            .unwrap()
            .markdown
            .views
            .entry((tab, group))
            .or_default()
            .clone()
    }
    fn settle(app: &App) {
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            app.poll_markdown();
            if {
                let rt = app.runtime.lock().unwrap();
                rt.markdown.job.is_none() && rt.markdown.image_job.is_none()
            } {
                break;
            }
            assert!(Instant::now() < deadline, "Markdown worker timed out");
            thread::sleep(Duration::from_millis(5));
        }
    }
    #[test]
    fn preview_tracks_unsaved_edits_undo_theme_moves_and_cleanup() {
        let app = App::with_document();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/preview-test.md");
        app.complete_open(
            0,
            path,
            "# Original\n\n![Panel](screenshots/panel-bottom.png)".into(),
            None,
        );
        let id = app.document_id();
        app.markdown_preview_to_side();
        settle(&app);
        let group = app.focused_group.get();
        assert_ne!(group, 0);
        assert!(!app.markdown_previewing(id, 0));
        assert!(app.markdown_previewing(id, group));
        assert!(!app.editable(id));
        let image = app.runtime.lock().unwrap().markdown.entries[&id]
            .document
            .images["screenshots/panel-bottom.png"]
            .result
            .as_ref()
            .unwrap()
            .clone();
        app.markdown_preview_to_side();
        assert_eq!(app.pane_order().len(), 2);
        app.focus_group(0);
        app.edit(
            id,
            &EditDelta {
                replaced_range: 2..10,
                inserted: "Changed".into(),
            },
        );
        settle(&app);
        {
            let rt = app.runtime.lock().unwrap();
            let entry = &rt.markdown.entries[&id];
            assert_eq!(entry.document.blocks[0].text(), "Changed");
            assert!(Arc::ptr_eq(
                &image,
                entry.document.images["screenshots/panel-bottom.png"]
                    .result
                    .as_ref()
                    .unwrap()
            ));
        }
        app.undo(false);
        settle(&app);
        assert_eq!(
            app.runtime.lock().unwrap().markdown.entries[&id]
                .document
                .blocks[0]
                .text(),
            "Original"
        );
        app.set_theme(Theme::Dark);
        settle(&app);
        assert_eq!(
            app.runtime.lock().unwrap().markdown.entries[&id].key.theme,
            Theme::Dark
        );
        app.focus_group(group);
        let tab = app.group_id(group).unwrap();
        app.move_tab(tab, group, 0, 0);
        assert!(app.markdown_previewing(id, 0));
        app.close_active();
        app.activate(id);
        assert!(app.editable(id));
        app.remove_document(id);
        assert!(
            !app.runtime
                .lock()
                .unwrap()
                .markdown
                .entries
                .contains_key(&id)
        );
        assert!(app.runtime.lock().unwrap().markdown.tabs.is_empty());
    }
    #[test]
    fn stale_worker_results_never_replace_latest_revision() {
        let app = App::with_document();
        let id = app.document_id();
        app.markdown_preview_to_side();
        let group = app.focused_group.get();
        app.focus_group(0);
        for i in 0..20 {
            let length = app
                .runtime
                .lock()
                .unwrap()
                .workspace
                .get(id)
                .unwrap()
                .editor
                .document
                .len();
            app.edit(
                id,
                &EditDelta {
                    replaced_range: 0..length,
                    inserted: format!("# Revision {i}"),
                },
            );
            app.poll_markdown();
        }
        settle(&app);
        assert_eq!(
            app.runtime.lock().unwrap().markdown.entries[&id]
                .document
                .blocks[0]
                .text(),
            "Revision 19"
        );
        app.focus_group(group);
        app.close_active();
        settle(&app);
        app.markdown_preview_to_side();
        settle(&app);
        assert!(
            app.runtime.lock().unwrap().markdown.entries[&id]
                .pending
                .is_empty()
        );
    }
    #[test]
    #[cfg(not(target_os = "scarlet"))]
    fn slow_image_response_does_not_delay_text_revisions() {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/slow.png", listener.local_addr().unwrap());
        let (started_tx, started_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(Duration::from_secs(5)))
                .unwrap();
            let mut request = [0; 4096];
            stream.read(&mut request).unwrap();
            started_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_secs(3));
            let bytes = std::fs::read(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/screenshots/panel-bottom.png"),
            )
            .unwrap();
            write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", bytes.len()).unwrap();
            stream.write_all(&bytes).unwrap();
        });
        let app = App::with_document();
        let id = app.document_id();
        let source = format!("# Before\n\n![Image]({url})");
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: source,
            },
        );
        app.markdown_preview_to_side();
        let deadline = Instant::now() + Duration::from_secs(3);
        while started_rx.try_recv().is_err() {
            app.poll_markdown();
            assert!(Instant::now() < deadline);
            thread::sleep(Duration::from_millis(5));
        }
        app.focus_group(0);
        app.edit(
            id,
            &EditDelta {
                replaced_range: 2..8,
                inserted: "After".into(),
            },
        );
        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            app.poll_markdown();
            if app.runtime.lock().unwrap().markdown.entries[&id]
                .document
                .blocks[0]
                .text()
                == "After"
            {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "text revision waited for image network I/O"
            );
            thread::sleep(Duration::from_millis(5));
        }
        release_tx.send(()).unwrap();
        settle(&app);
        server.join().unwrap();
        assert!(
            app.runtime.lock().unwrap().markdown.entries[&id]
                .document
                .images[&url]
                .result
                .is_ok()
        );
    }
    #[test]
    #[ignore = "manual native renderer review; writes KUREHA_MARKDOWN_PREVIEW PNG"]
    fn markdown_preview_screenshot() {
        use scarlet_ui::pipeline::RenderingPipeline;
        let app = App::new();
        app.set_theme(kureha_code::theme::preset("kureha").unwrap());
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/preview-test.md");
        app.complete_open(0, path, "# Kureha Code\n\nA **native Markdown preview** with live edits, tables, and images.\n\n> 本文も画像も、エディタの中で。\n\n## Getting started\n\n1. Open a Markdown file\n2. Choose **Open Markdown Preview**\n\n```rust\nfn main() {\n    println!(\"Hello, Kureha!\");\n}\n```\n\n| Feature | Status |\n| --- | --- |\n| Live preview | Ready |\n| Images | Embedded |\n\n![Editor panels](screenshots/panel-bottom.png)\n".into(), None);
        app.markdown_preview_to_side();
        settle(&app);
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(
            Window::new("Markdown preview", app.content())
                .decorated(false)
                .size(Size::new(1440.0, 1060.0))
                .create_element(),
        );
        pipeline.layout_initial();
        let buffer = pipeline.render().unwrap();
        let pixels: Vec<_> = buffer
            .as_slice()
            .iter()
            .flat_map(|pixel| {
                [
                    (pixel >> 16) as u8,
                    (pixel >> 8) as u8,
                    *pixel as u8,
                    (pixel >> 24) as u8,
                ]
            })
            .collect();
        image::save_buffer(
            std::env::var_os("KUREHA_MARKDOWN_PREVIEW").unwrap(),
            &pixels,
            buffer.width(),
            buffer.height(),
            image::ColorType::Rgba8,
        )
        .unwrap();
        pipeline.teardown();
    }
    #[test]
    fn preview_is_an_ordinary_tab_with_its_own_identity_and_document_lifetime() {
        let app = App::new();
        app.complete_open(0, "/tmp/first.md".into(), "# First".into(), None);
        let source = app.document_id();
        app.open_markdown_preview();
        let preview = app.group_id(0).unwrap();
        assert_ne!(source, preview);
        assert_eq!(
            app.runtime.lock().unwrap().group_tabs[0],
            vec![source, preview]
        );
        assert_eq!(app.runtime.lock().unwrap().workspace.documents.len(), 1);
        app.split_tab(preview, 0, tab_drag::Edge::Right);
        let right = app.focused_group.get();
        assert_eq!(app.group_id(right), Some(preview));
        assert_eq!(app.group_id(0), Some(source));
        app.move_tab(preview, right, 0, 1);
        assert_eq!(app.pane_order(), vec![0]);
        app.activate(source);
        app.edit(
            source,
            &EditDelta {
                replaced_range: 2..7,
                inserted: "Unsaved".into(),
            },
        );
        app.close_active(); // The preview retains the unsaved source without duplicating its editor.
        assert!(app.runtime.lock().unwrap().close.is_none());
        assert_eq!(app.group_id(0), Some(preview));
        settle(&app);
        assert_eq!(
            app.runtime.lock().unwrap().markdown.entries[&source]
                .document
                .blocks[0]
                .text(),
            "Unsaved"
        );
        app.close_active();
        assert!(
            app.runtime.lock().unwrap().close.is_some(),
            "last preview must protect unsaved text"
        );
        app.cancel_close();
        assert_eq!(app.group_id(0), Some(preview));
        app.markdown_source();
        assert_eq!(app.group_id(0), Some(source));
        app.activate(preview);
        app.close_active();
        assert_eq!(app.group_id(0), Some(source));
        assert!(app.runtime.lock().unwrap().markdown.tabs.is_empty());
    }
    #[test]
    fn preview_file_follow_and_pin_do_not_replace_other_tabs() {
        let app = App::new();
        app.complete_open(0, "/tmp/first.md".into(), "# First".into(), None);
        let first = app.document_id();
        app.markdown_preview_to_side();
        let group = app.focused_group.get();
        let tab = app.group_id(group).unwrap();
        settle(&app);
        app.complete_open(0, "/tmp/second.md".into(), "# Second".into(), None);
        settle(&app);
        let second = app.group_id(0).unwrap();
        assert_eq!(app.group_document(group), Some(second));
        app.preview_follow(group);
        app.focus_group(0);
        app.activate(first);
        settle(&app);
        assert_eq!(app.group_document(group), Some(second));
        app.preview_follow(group);
        settle(&app);
        assert_eq!(app.group_document(group), Some(first));
        app.complete_open(group, "/tmp/code.rs".into(), "fn main() {}".into(), None);
        assert!(app.runtime.lock().unwrap().group_tabs[group].contains(&tab));
        app.focus_group(group);
        app.activate(tab);
        assert!(app.markdown_previewing(first, group));
        app.close_active();
        assert!(app.runtime.lock().unwrap().workspace.get(first).is_some());
    }
    #[test]
    fn typing_and_async_preview_updates_do_not_rebuild_workbench() {
        use scarlet_ui::pipeline::RenderingPipeline;
        let app = App::new();
        app.complete_open(
            0,
            "/tmp/typing.md".into(),
            (0..100)
                .map(|i| format!("## Heading {i}\n\nParagraph with **bold** text.\n\n"))
                .collect(),
            None,
        );
        let id = app.document_id();
        app.edit(
            id,
            &EditDelta {
                replaced_range: 3..3,
                inserted: "Warm ".into(),
            },
        );
        app.markdown_preview_to_side();
        settle(&app);
        app.focus_group(0);
        let window = || {
            Window::new("Typing", app.content())
                .decorated(false)
                .size(Size::new(1200.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        pipeline.render_for_present().unwrap();
        let chrome = app.chrome.get();
        crate::markdown_view::metrics::take();
        let started = Instant::now();
        for _ in 0..5 {
            pipeline.handle_event(&Event::TextInputCommit {
                text: "字".into(),
                context_id: 1,
                serial: 1,
            });
            pipeline.render_for_present().unwrap();
            assert_eq!(app.chrome.get(), chrome);
        }
        let typed = started.elapsed();
        assert!(
            app.runtime
                .lock()
                .unwrap()
                .workspace
                .get(id)
                .unwrap()
                .editor
                .document
                .as_str()
                .contains("字字字字字")
        );
        assert!(
            crate::markdown_view::metrics::take().layouts.is_empty(),
            "typing re-laid out unchanged preview"
        );
        settle(&app);
        pipeline.render_for_present().unwrap();
        assert_eq!(app.chrome.get(), chrome, "async preview rebuilt workbench");
        assert!(
            app.runtime.lock().unwrap().markdown.entries[&id]
                .document
                .blocks
                .iter()
                .any(|block| block.text().contains("字字字字字"))
        );
        app.syntax_revision.update(|value| *value += 1);
        pipeline.render_for_present().unwrap();
        assert_eq!(
            app.chrome.get(),
            chrome,
            "highlight completion rebuilt workbench"
        );
        assert!(
            crate::markdown_view::metrics::take().wraps < 10,
            "unchanged paragraphs were remeasured"
        );
        eprintln!("Five mounted text commits: {typed:?}");
        pipeline.teardown();
    }

    #[test]
    fn mounted_preview_syncs_scroll_both_directions_without_feedback_and_can_disable_it() {
        use scarlet_ui::{
            event::{MouseEvent, ScrollSource, WheelPhase},
            pipeline::RenderingPipeline,
        };
        let app = App::new();
        app.complete_open(
            0,
            "/tmp/long.md".into(),
            (0..150)
                .map(|i| format!("## Heading {i}\n\nParagraph with some text.\n\n"))
                .collect(),
            None,
        );
        let id = app.document_id();
        app.markdown_preview_to_side();
        let group = app.focused_group.get();
        settle(&app);
        let position = preview_state(&app, group).position;
        let window = || {
            Window::new("Preview sync", app.content())
                .decorated(false)
                .size(Size::new(1200.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        let revision = std::cell::Cell::new(app.chrome.get());
        let pump = |pipeline: &mut RenderingPipeline| {
            for _ in 0..6 {
                app.poll_markdown();
                if revision.replace(app.chrome.get()) != app.chrome.get() {
                    pipeline
                        .element_tree_mut()
                        .root_mut()
                        .unwrap()
                        .update(&window());
                }
                pipeline.render_for_present().unwrap();
            }
        };
        pump(&mut pipeline);
        assert!(position.lock().unwrap().viewport.size.height > 100.0);
        let bindings = app.bindings_for(id, 0).unwrap();
        let chrome_before = app.chrome.get();
        crate::markdown_view::metrics::take();
        bindings.scroll.set(TextViewScroll { x: 0.0, y: 500.0 });
        pump(&mut pipeline);
        assert_eq!(
            app.chrome.get(),
            chrome_before,
            "source scroll rebuilt the workbench"
        );
        assert!(
            crate::markdown_view::metrics::take().layouts.is_empty(),
            "source scroll remeasured Markdown"
        );
        let y = position.lock().unwrap().viewport.origin.y;
        assert!(y > 300.0, "source scroll did not reveal preview: {y}");
        assert!(
            preview_state(&app, group).requested_y.is_none(),
            "reveal never settled"
        );
        let source_before = bindings.scroll.get().y;
        fn viewport_point(e: &dyn Element, origin: Point) -> Option<Point> {
            let point = Point::new(origin.x + e.position().x, origin.y + e.position().y);
            if e.type_name_debug().contains("ScrollViewRenderObject") && contains_markdown(e) {
                return Some(Point::new(point.x + 30.0, point.y + 80.0));
            }
            e.children()
                .iter()
                .find_map(|child| viewport_point(child.as_ref(), point))
        }
        fn contains_markdown(e: &dyn Element) -> bool {
            e.type_name_debug().contains("markdown_view::Renderer")
                || e.children()
                    .iter()
                    .any(|child| contains_markdown(child.as_ref()))
        }
        let point = viewport_point(pipeline.element_tree().root().unwrap(), Point::ZERO)
            .expect("preview viewport");
        let wheel = |pipeline: &mut RenderingPipeline| {
            assert!(
                pipeline.handle_event(&Event::Mouse(MouseEvent::Wheel {
                    delta_x: 0,
                    delta_y: -240,
                    x: point.x as i32,
                    y: point.y as i32,
                    phase: WheelPhase::Moved,
                    source: ScrollSource::Trackpad
                })),
                "wheel unhandled at {point:?}"
            );
            pipeline.render();
        };
        // No click/focus change: scrolling either side must drive the other side.
        wheel(&mut pipeline);
        pump(&mut pipeline);
        assert!(
            bindings.scroll.get().y > source_before + 30.0,
            "preview did not move source: source {} -> {}, preview {} -> {}",
            source_before,
            bindings.scroll.get().y,
            y,
            position.lock().unwrap().viewport.origin.y
        );
        let stable = bindings.scroll.get().y;
        pump(&mut pipeline);
        assert!(
            (stable - bindings.scroll.get().y).abs() < 1.0,
            "scroll feedback loop"
        );
        app.preview_scroll_sync(group);
        wheel(&mut pipeline);
        pump(&mut pipeline);
        assert!(
            (stable - bindings.scroll.get().y).abs() < 1.0,
            "disabled sync moved source"
        );
        app.request_preview_scroll(group, 2000.0);
        app.focus_group(0);
        let length = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .editor
            .document
            .len();
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..length,
                inserted: "# Short".into(),
            },
        );
        settle(&app);
        pump(&mut pipeline);
        assert!(
            preview_state(&app, group).requested_y.is_none(),
            "shrinking document left a stale reveal"
        );
        assert_eq!(position.lock().unwrap().viewport.origin.y, 0.0);
        pipeline.teardown();
    }
    #[test]
    #[ignore = "release CPU submission profiling; no GPU timing claim"]
    fn preview_frame_profile() {
        use scarlet_ui::{
            compositor::DamageRect,
            pipeline::RenderingPipeline,
            renderer::{BackendFrame, PaintBackend, PaintContext},
        };
        use std::{cell::Cell, rc::Rc};
        struct CountBackend(Rc<Cell<usize>>);
        impl PaintBackend for CountBackend {
            fn supports_retained_display_lists(&self) -> bool {
                true
            }
            fn resize(&mut self, _: Size, _: u32) {}
            fn render<'a>(
                &'a mut self,
                _: &PaintContext<'_>,
                _: Color,
                _: Option<&[Rect]>,
                _: Option<&[DamageRect]>,
            ) -> scarlet_ui::Result<BackendFrame<'a>> {
                self.0.set(self.0.get() + 1);
                Ok(BackendFrame::External)
            }
        }
        let app = App::new();
        app.complete_open(
            0,
            "/tmp/profile.md".into(),
            (0..150)
                .map(|i| format!("## Heading {i}\n\nParagraph with **bold** and some text.\n\n"))
                .collect(),
            None,
        );
        app.markdown_preview_to_side();
        settle(&app);
        let window = || {
            Window::new("Profile", app.content())
                .decorated(false)
                .size(Size::new(1200.0, 760.0))
        };
        let count = Rc::new(Cell::new(0));
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_backend(Box::new(CountBackend(count.clone())));
        pipeline.set_root(window().create_element());
        pipeline.layout_initial();
        let tick = |pipeline: &mut RenderingPipeline| {
            let chrome = app.chrome.get();
            app.poll_markdown();
            if chrome != app.chrome.get() {
                pipeline
                    .element_tree_mut()
                    .root_mut()
                    .unwrap()
                    .update(&window());
            }
            pipeline.render_for_present().unwrap();
        };
        for _ in 0..12 {
            tick(&mut pipeline);
        }
        for mode in ["idle", "preview-scroll", "source-scroll"] {
            crate::markdown_view::metrics::take();
            count.set(0);
            let started = Instant::now();
            for _ in 0..30 {
                if mode != "idle" {
                    pipeline.handle_event(&Event::Mouse(scarlet_ui::event::MouseEvent::Wheel {
                        delta_x: 0,
                        delta_y: -80,
                        x: if mode == "preview-scroll" { 1000 } else { 500 },
                        y: 350,
                        phase: scarlet_ui::event::WheelPhase::Moved,
                        source: scarlet_ui::event::ScrollSource::Trackpad,
                    }));
                }
                tick(&mut pipeline);
                tick(&mut pipeline);
                tick(&mut pipeline);
            }
            let m = crate::markdown_view::metrics::take();
            eprintln!(
                "PROFILE {mode}: elapsed={:?} presents={} layouts={} wraps={} paints={} widths={:?}",
                started.elapsed(),
                count.get(),
                m.layouts.len(),
                m.wraps,
                m.paints,
                &m.layouts[..m.layouts.len().min(20)]
            );
        }
        pipeline.teardown();
    }
}
