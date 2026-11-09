//! Independent preview panes with coalesced background parsing and source synchronization.
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
pub(super) struct PreviewPane {
    pub source: usize,
    pub follow_file: bool,
    pub sync_position: bool,
    pub position: Arc<Mutex<crate::markdown_view::PreviewPosition>>,
    pub requested_y: Option<f32>,
    last_source_y: f32,
    last_preview_y: f32,
    last_geometry: u64,
    last_document: Option<DocumentId>,
}
#[derive(Default)]
pub(super) struct Previews {
    pub panels: BTreeMap<usize, PreviewPane>,
    entries: BTreeMap<DocumentId, Entry>,
    job: Option<Job>,
    image_job: Option<ImageJob>,
    links: Vec<(DocumentId, String)>,
}
impl Previews {
    pub fn remove(&mut self, id: DocumentId) {
        self.entries.remove(&id);
        self.links.retain(|(document, _)| *document != id);
        if self.job.as_ref().is_some_and(|job| job.id == id) {
            self.job = None;
        }
    }
    pub fn retire_pane(&mut self, group: usize) {
        self.panels.remove(&group);
    }
}
impl App {
    pub(super) fn is_preview_pane(&self, group: usize) -> bool {
        self.runtime
            .lock()
            .unwrap()
            .markdown
            .panels
            .contains_key(&group)
    }
    pub(super) fn editor_pane_order(&self) -> Vec<usize> {
        let panes = self.pane_order();
        let rt = self.runtime.lock().unwrap();
        panes
            .into_iter()
            .filter(|g| !rt.markdown.panels.contains_key(g))
            .collect()
    }
    pub(super) fn focus_editor(&self) {
        let group = self.focused_group.get();
        let source = self
            .runtime
            .lock()
            .unwrap()
            .markdown
            .panels
            .get(&group)
            .map(|p| p.source);
        if let Some(source) = source {
            let editors = self.editor_pane_order();
            if let Some(source) = editors.iter().find(|g| **g == source).or(editors.first()) {
                self.focus_group(*source);
            }
        }
    }
    #[cfg(test)]
    pub(super) fn markdown_previewing(&self, id: DocumentId, group: usize) -> bool {
        self.is_preview_pane(group) && self.group_id(group) == Some(id)
    }
    pub(super) fn open_markdown_preview(&self) {
        self.markdown_preview_to_side();
    }
    pub(super) fn markdown_preview_to_side(&self) {
        if self.blocked() {
            return;
        }
        self.focus_editor();
        let source = self.focused_group.get();
        let existing = self
            .runtime
            .lock()
            .unwrap()
            .markdown
            .panels
            .iter()
            .find_map(|(group, pane)| (pane.source == source).then_some(*group));
        if let Some(group) = existing {
            self.focus_group(group);
            return;
        }
        let id = self.active_id().filter(|id| {
            self.runtime
                .lock()
                .unwrap()
                .workspace
                .get(*id)
                .is_some_and(|d| {
                    d.path.as_deref().is_none_or(markdown::is_markdown) && d.image.is_none()
                })
        });
        self.dismiss_language_panel();
        self.remember_pane_sizes();
        let group = self.allocate_empty_group();
        self.pane_layout
            .update(|layout| layout.split(source, group, tab_drag::Edge::Right));
        self.runtime.lock().unwrap().markdown.panels.insert(
            group,
            PreviewPane {
                source,
                follow_file: true,
                sync_position: true,
                position: Default::default(),
                requested_y: None,
                last_source_y: -1.0,
                last_preview_y: 0.0,
                last_geometry: 0,
                last_document: None,
            },
        );
        if let Some(id) = id {
            self.set_group_id(group, id);
        }
        self.focus_group(group);
        self.poll_markdown();
        self.changed();
    }
    pub(super) fn close_markdown_preview(&self, group: usize) {
        let pane = self
            .runtime
            .lock()
            .unwrap()
            .markdown
            .panels
            .get(&group)
            .cloned();
        let Some(pane) = pane else {
            return;
        };
        self.runtime.lock().unwrap().markdown.retire_pane(group);
        self.panes.update(|panes| panes[group] = None);
        if let Some(layout) = self.pane_layout.get().retain(&|g| g != group) {
            self.pane_layout.set(layout);
        }
        if self.focused_group.get() == group {
            let target = if self.group_exists(pane.source) {
                pane.source
            } else {
                self.editor_pane_order()[0]
            };
            self.focus_group(target);
        }
        self.changed();
    }
    fn sync_preview_files(&self) {
        let editors = self.editor_pane_order();
        let panes = self.panes.get();
        let mut changes = Vec::new();
        {
            let mut rt = self.runtime.lock().unwrap();
            let previews: Vec<_> = rt
                .markdown
                .panels
                .iter()
                .map(|(g, p)| (*g, p.clone()))
                .collect();
            for (group, mut preview) in previews {
                if !editors.contains(&preview.source) {
                    let Some(source) = editors.first() else {
                        continue;
                    };
                    preview.source = *source;
                    rt.markdown.panels.insert(group, preview.clone());
                }
                if preview.follow_file {
                    let source = panes[preview.source].filter(|id| {
                        rt.workspace.get(*id).is_some_and(|d| {
                            d.image.is_none() && d.path.as_deref().is_none_or(markdown::is_markdown)
                        })
                    });
                    if source != panes[group] {
                        changes.push((group, source));
                    }
                }
            }
        }
        if !changes.is_empty() {
            {
                let mut rt = self.runtime.lock().unwrap();
                for (group, _) in &changes {
                    let pane = rt.markdown.panels.get_mut(group).unwrap();
                    pane.requested_y = None;
                    pane.last_document = None;
                }
            }
            self.panes.update(|panes| {
                for (group, id) in changes {
                    panes[group] = id;
                }
            });
        }
    }
    fn preview_follow(&self, group: usize) {
        if let Some(pane) = self.runtime.lock().unwrap().markdown.panels.get_mut(&group) {
            pane.follow_file = !pane.follow_file;
        }
        self.sync_preview_files();
        self.changed();
    }
    fn preview_scroll_sync(&self, group: usize) {
        if let Some(pane) = self.runtime.lock().unwrap().markdown.panels.get_mut(&group) {
            pane.sync_position = !pane.sync_position;
            pane.last_document = None;
            pane.requested_y = None;
        }
        self.changed();
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
            .filter(|(_, group)| rt.markdown.panels.contains_key(group))
            .filter_map(|(id, _)| {
                let doc = rt.workspace.get(*id)?;
                Some((
                    *id,
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
            self.changed();
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
        let source = {
            let mut rt = self.runtime.lock().unwrap();
            let Some(pane) = rt.markdown.panels.get_mut(&group) else {
                return;
            };
            let position = pane.position.lock().unwrap();
            let max = (position.mapping.last().map_or(0.0, |p| p.1)
                - position.viewport.size.height)
                .max(0.0);
            let y = y.clamp(0.0, max);
            pane.requested_y = Some(y);
            pane.sync_position
                .then(|| (pane.source, position.source_line(y)))
        };
        if let Some((source, line)) = source {
            if let Some(id) = self
                .group_id(group)
                .filter(|id| self.group_id(source) == Some(*id))
            {
                if let Some(bindings) = self.bindings_for(id, source) {
                    let scroll = bindings.scroll.get();
                    bindings.scroll.set(TextViewScroll {
                        x: scroll.x,
                        y: line * self.preview_source_line_height(),
                    });
                }
            }
        }
        self.changed();
    }
    fn sync_preview_position(&self) {
        let panes: Vec<_> = self
            .runtime
            .lock()
            .unwrap()
            .markdown
            .panels
            .iter()
            .map(|(g, p)| (*g, p.clone()))
            .collect();
        if panes.is_empty() {
            return;
        }
        let line_height = self.preview_source_line_height();
        let mut changed = false;
        for (group, snapshot) in panes {
            // A document can shrink while a reveal is queued. Clamp against the
            // latest layout, including pinned previews whose source is elsewhere.
            if snapshot.requested_y.is_some() {
                let mut rt = self.runtime.lock().unwrap();
                let pane = rt.markdown.panels.get_mut(&group).unwrap();
                let position = pane.position.lock().unwrap();
                if position.viewport.size.height > 0.0 {
                    let max = (position.mapping.last().map_or(0.0, |p| p.1)
                        - position.viewport.size.height)
                        .max(0.0);
                    let target = pane.requested_y.unwrap().clamp(0.0, max);
                    if (position.viewport.origin.y - target).abs() < 1.0 {
                        pane.requested_y = None;
                        pane.last_preview_y = position.viewport.origin.y;
                        pane.last_geometry = position.geometry;
                        changed = true;
                    } else if pane.requested_y != Some(target) {
                        pane.requested_y = Some(target);
                        changed = true;
                    }
                }
                continue;
            }
            let Some(id) = self.group_id(group) else {
                continue;
            };
            if self.group_id(snapshot.source) != Some(id) {
                continue;
            }
            let Some(bindings) = self.bindings_for(id, snapshot.source) else {
                continue;
            };
            let source_scroll = bindings.scroll.get();
            let mut reverse = None;
            {
                let mut rt = self.runtime.lock().unwrap();
                let Some(model) = rt
                    .markdown
                    .entries
                    .get(&id)
                    .filter(|e| {
                        rt.workspace
                            .get(id)
                            .is_some_and(|d| d.editor.revision() == e.key.revision)
                    })
                    .map(|e| Arc::as_ptr(&e.document) as usize)
                else {
                    continue;
                };
                let pane = rt.markdown.panels.get_mut(&group).unwrap();
                let position = pane.position.lock().unwrap();
                if position.model != model || position.viewport.size.height <= 0.0 {
                    continue;
                }
                let y = position.viewport.origin.y;
                let initial = pane.last_document != Some(id);
                if pane.sync_position
                    && (initial
                        || (source_scroll.y - pane.last_source_y).abs() > 0.5
                        || pane.last_geometry != position.geometry
                            && (y - pane.last_preview_y).abs() <= 0.5)
                {
                    let max = (position.mapping.last().map_or(0.0, |p| p.1)
                        - position.viewport.size.height)
                        .max(0.0);
                    let target = position
                        .preview_y(source_scroll.y / line_height)
                        .clamp(0.0, max);
                    if (y - target).abs() > 1.0 {
                        pane.requested_y = Some(target);
                        changed = true;
                    }
                } else if pane.sync_position && (y - pane.last_preview_y).abs() > 0.5 {
                    reverse = Some(position.source_line(y) * line_height);
                }
                pane.last_source_y = reverse.unwrap_or(source_scroll.y);
                pane.last_preview_y = y;
                pane.last_geometry = position.geometry;
                pane.last_document = Some(id);
            }
            if let Some(y) = reverse {
                bindings.scroll.set(TextViewScroll {
                    x: source_scroll.x,
                    y,
                });
            }
        }
        if changed {
            self.changed();
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
    pub(super) fn markdown_pane(&self, group: usize) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let pane = self.runtime.lock().unwrap().markdown.panels[&group].clone();
        let id = self.group_id(group);
        let title = id
            .and_then(|id| {
                self.runtime
                    .lock()
                    .unwrap()
                    .workspace
                    .get(id)
                    .map(|d| d.name())
            })
            .unwrap_or_else(|| "Markdown Preview".into());
        let close = self.clone();
        let follow = self.clone();
        let sync = self.clone();
        let source = self.clone();
        let focus = self.clone();
        vstack! {
            hstack! {
                Text::new(title).font_size(12.0).color(theme.colors().foreground), Spacer::new(),
                Button::new("File Sync").header_style().font_size(11.0).padding(4.0).text_color(theme.colors().foreground).background_color(if pane.follow_file { theme.colors().selection } else { Color::TRANSPARENT }).on_click(move || follow.preview_follow(group)),
                Button::new("Scroll Sync").header_style().font_size(11.0).padding(4.0).text_color(theme.colors().foreground).background_color(if pane.sync_position { theme.colors().selection } else { Color::TRANSPARENT }).on_click(move || sync.preview_scroll_sync(group)),
                Button::new("Source").header_style().font_size(11.0).padding(4.0).text_color(theme.colors().muted).on_click(move || { source.focus_group(pane.source); if let Some(id) = id { source.activate(id); } }),
                Button::new("×").header_style().font_size(13.0).padding(4.0).text_color(theme.colors().muted).on_click(move || close.close_markdown_preview(group)),
            }.spacing(5.0).padding(10.0).clip(),
            match id {
                Some(id) => Either::A(self.markdown_view(id, group)),
                None => Either::B(VStack::new(()).frame(f32::INFINITY, f32::INFINITY).background(theme.editor_colors().background)),
            },
        }.spacing(0.0).frame(f32::INFINITY, f32::INFINITY).on_event_capture(move |event| {
            if matches!(event, Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })) { focus.focus_group(group); }
            false
        })
    }
    pub(super) fn markdown_view(&self, id: DocumentId, group: usize) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let pane = self.runtime.lock().unwrap().markdown.panels[&group].clone();
        let document = self
            .runtime
            .lock()
            .unwrap()
            .markdown
            .entries
            .get(&id)
            .map(|entry| entry.document.clone())
            .unwrap_or_default();
        let app = self.clone();
        let focus = self.clone();
        let scroll = self.clone();
        let viewport_height = pane.position.lock().unwrap().viewport.size.height;
        let body = ScrollView::new(crate::markdown_view::MarkdownView {
            document,
            theme: theme.clone(),
            font: self.document_font(),
            position: pane.position.clone(),
            on_scroll: Arc::new(move |y| scroll.request_preview_scroll(group, y)),
            on_link: Arc::new(move |link| {
                app.runtime.lock().unwrap().markdown.links.push((id, link))
            }),
        })
        .scrollbar_color(theme.colors().muted);
        let body = if let Some(y) = pane.requested_y {
            body.scroll_to_vertical_range(y, y + viewport_height.max(1.0))
        } else {
            body
        };
        Surface::section(
            body.frame(f32::INFINITY, f32::INFINITY)
                .clip()
                .key(format!("markdown-{id}")),
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
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
        app.move_tab(id, group, 0, 0);
        assert!(app.markdown_previewing(id, group));
        assert!(!app.markdown_previewing(id, 0));
        app.close_markdown_preview(group);
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
        assert!(app.runtime.lock().unwrap().markdown.panels.is_empty());
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
        app.close_markdown_preview(group);
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
    fn independent_preview_follows_files_can_pin_and_never_owns_editor_tabs() {
        let app = App::new();
        app.complete_open(0, "/tmp/first.md".into(), "# First".into(), None);
        let first = app.document_id();
        app.markdown_preview_to_side();
        let preview = app.focused_group.get();
        settle(&app);
        app.complete_open(0, "/tmp/second.md".into(), "# Second".into(), None);
        settle(&app);
        let second = app.group_id(0).unwrap();
        assert_eq!(app.group_id(preview), Some(second));
        app.preview_follow(preview);
        app.activate(first); // Activation from the preview must route to its source editor.
        settle(&app);
        assert_eq!(app.focused_group.get(), 0);
        assert_eq!(app.group_id(preview), Some(second));
        assert!(app.runtime.lock().unwrap().group_tabs[preview].is_empty());
        app.preview_follow(preview);
        settle(&app);
        assert_eq!(app.group_id(preview), Some(first));
        app.complete_open(0, "/tmp/code.rs".into(), "fn main() {}".into(), None);
        settle(&app);
        assert_eq!(app.group_id(preview), None);
        assert!(app.group_exists(preview));
        app.activate(first);
        settle(&app);
        app.focus_group(preview);
        app.close_active();
        assert!(!app.group_exists(preview));
        assert!(app.runtime.lock().unwrap().workspace.get(first).is_some());
        assert_eq!(app.group_id(0), Some(first));
        app.markdown_preview_to_side();
        let preview = app.focused_group.get();
        let documents: Vec<_> = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .documents
            .iter()
            .map(|d| d.id)
            .collect();
        for id in documents {
            app.remove_document(id);
        }
        settle(&app);
        assert_eq!(app.editor_pane_order(), vec![0]);
        assert_eq!(app.group_id(0), None);
        assert_eq!(app.group_id(preview), None);
        assert!(app.runtime.lock().unwrap().workspace.documents.is_empty());
        app.close_markdown_preview(preview);
        assert_eq!(app.pane_order(), vec![0]);
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
        let position = app.runtime.lock().unwrap().markdown.panels[&group]
            .position
            .clone();
        let window = || {
            Window::new("Preview sync", app.content())
                .decorated(false)
                .size(Size::new(1200.0, 760.0))
        };
        let mut pipeline = RenderingPipeline::new();
        pipeline.set_paint_enabled(false);
        pipeline.set_root(window().create_element());
        let pump = |pipeline: &mut RenderingPipeline| {
            for _ in 0..6 {
                pipeline
                    .element_tree_mut()
                    .root_mut()
                    .unwrap()
                    .update(&window());
                pipeline.layout_initial();
                pipeline.render();
                app.poll_markdown();
            }
        };
        pump(&mut pipeline);
        assert!(position.lock().unwrap().viewport.size.height > 100.0);
        let bindings = app.bindings_for(id, 0).unwrap();
        bindings.scroll.set(TextViewScroll { x: 0.0, y: 500.0 });
        pump(&mut pipeline);
        let y = position.lock().unwrap().viewport.origin.y;
        assert!(y > 300.0, "source scroll did not reveal preview: {y}");
        assert!(
            app.runtime.lock().unwrap().markdown.panels[&group]
                .requested_y
                .is_none(),
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
            app.runtime.lock().unwrap().markdown.panels[&group]
                .requested_y
                .is_none(),
            "shrinking document left a stale reveal"
        );
        assert_eq!(position.lock().unwrap().viewport.origin.y, 0.0);
        pipeline.teardown();
    }
}
