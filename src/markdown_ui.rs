//! Per-pane source/preview mode with a single coalescing background preview job.
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
#[derive(Default)]
pub(super) struct Previews {
    pub modes: BTreeSet<(DocumentId, usize)>,
    scroll_targets: BTreeMap<(DocumentId, usize), State<Option<usize>>>,
    entries: BTreeMap<DocumentId, Entry>,
    job: Option<Job>,
    image_job: Option<ImageJob>,
    links: Vec<(DocumentId, String)>,
}
impl Previews {
    pub fn remove(&mut self, id: DocumentId) {
        self.modes.retain(|(document, _)| *document != id);
        self.entries.remove(&id);
        self.scroll_targets
            .retain(|(document, _), _| *document != id);
        self.links.retain(|(document, _)| *document != id);
        if self.job.as_ref().is_some_and(|job| job.id == id) {
            self.job = None;
        }
    }
    pub fn retire_pane(&mut self, group: usize) {
        self.modes.retain(|(_, g)| *g != group);
        self.scroll_targets.retain(|(_, g), _| *g != group);
    }
    pub fn move_pane(&mut self, id: DocumentId, from: usize, to: usize, copy: bool) {
        let scroll = if copy {
            self.scroll_targets
                .get(&(id, from))
                .map(|s| State::new(generate_state_id(), s.get()))
        } else {
            self.scroll_targets.remove(&(id, from))
        };
        if let Some(scroll) = scroll {
            self.scroll_targets.insert((id, to), scroll);
        }
        let preview = if copy {
            self.modes.contains(&(id, from))
        } else {
            self.modes.remove(&(id, from))
        };
        if preview {
            self.modes.insert((id, to));
        } else {
            self.modes.remove(&(id, to));
        }
    }
}
impl App {
    pub(super) fn markdown_previewing(&self, id: DocumentId, group: usize) -> bool {
        self.runtime
            .lock()
            .unwrap()
            .markdown
            .modes
            .contains(&(id, group))
    }
    fn can_preview_markdown(&self, id: DocumentId) -> bool {
        self.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .is_some_and(|doc| {
                doc.image.is_none() && doc.path.as_deref().is_none_or(markdown::is_markdown)
            })
    }
    pub(super) fn toggle_markdown_preview(&self) {
        if self.blocked() {
            return;
        }
        let id = self.active_id();
        let group = self.focused_group.get();
        if !self.markdown_previewing(id, group) && !self.can_preview_markdown(id) {
            self.status
                .set("Open a Markdown document to preview it".into());
            return;
        }
        self.dismiss_language_panel();
        self.invalidate_clipboard();
        let mut rt = self.runtime.lock().unwrap();
        if !rt.markdown.modes.remove(&(id, group)) {
            rt.markdown.modes.insert((id, group));
        }
        drop(rt);
        self.poll_markdown();
        self.changed();
    }
    pub(super) fn markdown_preview_to_side(&self) {
        if self.blocked() {
            return;
        }
        let id = self.active_id();
        if !self.can_preview_markdown(id) {
            self.status
                .set("Open a Markdown document to preview it".into());
            return;
        }
        let existing = self
            .runtime
            .lock()
            .unwrap()
            .markdown
            .modes
            .iter()
            .find_map(|(doc, group)| (*doc == id).then_some(*group));
        if let Some(group) = existing {
            self.set_group_id(group, id);
            self.focus_group(group);
            self.changed();
            return;
        }
        self.dismiss_language_panel();
        let group = self.add_group(id, self.focused_group.get(), tab_drag::Edge::Right);
        self.runtime
            .lock()
            .unwrap()
            .markdown
            .modes
            .insert((id, group));
        self.focus_group(group);
        self.poll_markdown();
        self.changed();
    }
    pub(super) fn refresh_markdown_preview(&self) {
        let id = self.active_id();
        self.runtime.lock().unwrap().markdown.entries.remove(&id);
        self.poll_markdown();
        self.changed();
    }
    pub(super) fn poll_markdown(&self) {
        let visible: BTreeSet<_> = self
            .panes
            .get()
            .iter()
            .enumerate()
            .filter_map(|(group, id)| id.map(|id| (id, group)))
            .collect();
        let theme = self.theme.get();
        let mut rt = self.runtime.lock().unwrap();
        let wanted: BTreeMap<_, _> = rt
            .markdown
            .modes
            .intersection(&visible)
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
    pub(super) fn markdown_view(&self, id: DocumentId, group: usize) -> impl View + Clone + use<> {
        let theme = self.theme.get();
        let scroll_target = self
            .runtime
            .lock()
            .unwrap()
            .markdown
            .scroll_targets
            .entry((id, group))
            .or_insert_with(|| State::new(generate_state_id(), None))
            .clone();
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
        let source = self.clone();
        let focus = self.clone();
        Surface::section(vstack! {
            hstack! { Spacer::new(), Button::new("Source").header_style().font_size(11.0).padding(5.0)
                .text_color(theme.colors().muted).on_click(move || { source.focus_group(group); source.toggle_markdown_preview(); }), }.padding(4.0),
            ScrollView::new(crate::markdown_view::MarkdownView { document, theme: theme.clone(), font: self.document_font(), scroll_target: scroll_target.clone(), on_link: Arc::new(move |link| app.runtime.lock().unwrap().markdown.links.push((id, link))) })
                .scroll_to_index_state(scroll_target, 1.0).scrollbar_color(theme.colors().muted).frame(f32::INFINITY, f32::INFINITY).clip(),
        }.spacing(0.0).frame(f32::INFINITY, f32::INFINITY))
            .fill(theme.editor_colors().background).corner_radius(12.0).clip_content(true)
            .padding_insets(EdgeInsets::new(4.0, 0.0, 4.0, 4.0))
            .on_event_capture(move |event| {
                if matches!(event, Event::Mouse(scarlet_ui::event::MouseEvent::ButtonPressed { .. })) { focus.focus_group(group); }
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
        let app = App::new();
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("docs/preview-test.md");
        app.complete_open(
            0,
            path,
            "# Original\n\n![Panel](screenshots/panel-bottom.png)".into(),
            None,
        );
        let id = app.active_id();
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
        assert!(app.markdown_previewing(id, 0));
        app.toggle_markdown_preview();
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
        assert!(app.runtime.lock().unwrap().markdown.modes.is_empty());
    }
    #[test]
    fn stale_worker_results_never_replace_latest_revision() {
        let app = App::new();
        let id = app.active_id();
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
        app.toggle_markdown_preview();
        settle(&app);
        app.toggle_markdown_preview();
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
        let app = App::new();
        let id = app.active_id();
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
        app.complete_open(0, path, "# Kureha Code\n\nA **native Markdown preview** with live edits, tables, and images.\n\n> 本文も画像も、エディタの中で。\n\n## Getting started\n\n1. Open a Markdown file\n2. Choose **Open Markdown Preview to Side**\n\n```rust\nfn main() {\n    println!(\"Hello, Kureha!\");\n}\n```\n\n| Feature | Status |\n| --- | --- |\n| Live preview | Ready |\n| Images | Embedded |\n\n![Editor panels](screenshots/panel-bottom.png)\n".into(), None);
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
}
