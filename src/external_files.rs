//! Poll disk metadata off the UI thread and reload only pristine snapshots.
use super::*;
use std::time::{Duration, Instant, SystemTime};

#[derive(Clone, PartialEq, Eq)]
struct Stamp {
    modified: SystemTime,
    len: u64,
}

#[derive(Clone)]
struct Snapshot {
    id: DocumentId,
    revision: u64,
    path: PathBuf,
    disk: Arc<str>,
    stamp: Option<Stamp>,
}

struct Update {
    snapshot: Snapshot,
    stamp: Stamp,
    text: Option<String>,
}

#[derive(Default)]
pub(crate) struct Watcher {
    last_poll: Option<Instant>,
    stamps: BTreeMap<DocumentId, (PathBuf, Stamp)>,
    pending: Option<mpsc::Receiver<Vec<Update>>>,
}

fn stamp(path: &Path) -> Option<Stamp> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(Stamp {
        modified: meta.modified().ok()?,
        len: meta.len(),
    })
}

fn scan(snapshots: Vec<Snapshot>) -> Vec<Update> {
    snapshots
        .into_iter()
        .filter_map(|snapshot| {
            let before = stamp(&snapshot.path)?;
            if snapshot.stamp.as_ref() == Some(&before) {
                return None;
            }
            let text = files::read(&snapshot.path).ok()?;
            // A writer may still be replacing the file. Retry on the next poll.
            if stamp(&snapshot.path).as_ref() != Some(&before) {
                return None;
            }
            let text = (text.as_str() != snapshot.disk.as_ref()).then_some(text);
            Some(Update {
                snapshot,
                stamp: before,
                text,
            })
        })
        .collect()
}

fn apply(workspace: &mut Workspace, update: &Update) -> Option<bool> {
    let old = &update.snapshot;
    let doc = workspace.get_mut(old.id)?;
    if doc.path.as_ref() != Some(&old.path)
        || doc.editor.revision() != old.revision
        || doc.editor.dirty()
        || doc.disk_text.as_deref() != Some(old.disk.as_ref())
    {
        return None;
    }
    let Some(text) = &update.text else {
        return Some(false);
    };
    doc.editor.reload(text);
    doc.crlf = text.contains("\r\n");
    doc.disk_text = Some(Arc::from(text.as_str()));
    Some(true)
}

impl App {
    pub(crate) fn poll_external_files(&self) {
        let mut changed = Vec::new();
        {
            let mut rt = self.runtime.lock().unwrap();
            // File jobs own their snapshots until completion, including Save As.
            if rt.job.is_some() || rt.close.is_some() {
                return;
            }
            let updates = rt.external_files.pending.as_ref().map(|rx| rx.try_recv());
            match updates {
                Some(Ok(updates)) => {
                    rt.external_files.pending = None;
                    for update in updates {
                        if let Some(reloaded) = apply(&mut rt.workspace, &update) {
                            rt.external_files
                                .stamps
                                .insert(update.snapshot.id, (update.snapshot.path, update.stamp));
                            if reloaded {
                                changed.push(update.snapshot.id);
                            }
                        }
                    }
                }
                Some(Err(mpsc::TryRecvError::Disconnected)) => rt.external_files.pending = None,
                _ => {}
            }
            if rt.external_files.pending.is_none()
                && rt
                    .external_files
                    .last_poll
                    .is_none_or(|time| time.elapsed() >= Duration::from_secs(1))
            {
                let ids: BTreeSet<_> = rt.workspace.documents.iter().map(|doc| doc.id).collect();
                rt.external_files.stamps.retain(|id, _| ids.contains(id));
                let snapshots: Vec<_> = rt
                    .workspace
                    .documents
                    .iter()
                    .filter_map(|doc| {
                        if doc.image.is_some() || doc.editor.dirty() {
                            return None;
                        }
                        let path = doc.path.clone()?;
                        Some(Snapshot {
                            id: doc.id,
                            revision: doc.editor.revision(),
                            disk: doc.disk_text.clone()?,
                            stamp: rt
                                .external_files
                                .stamps
                                .get(&doc.id)
                                .filter(|(old, _)| old == &path)
                                .map(|(_, stamp)| stamp.clone()),
                            path,
                        })
                    })
                    .collect();
                rt.external_files.last_poll = Some(Instant::now());
                if !snapshots.is_empty() {
                    let (tx, rx) = mpsc::channel();
                    rt.external_files.pending = Some(rx);
                    thread::spawn(move || {
                        let _ = tx.send(scan(snapshots));
                    });
                }
            }
        }
        if !changed.is_empty() {
            self.invalidate_clipboard();
            self.dismiss_language_panel();
            for id in changed {
                self.sync_editor(id);
            }
            self.update_position();
            self.changed();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn polling_publishes_external_changes_to_existing_editor_bindings() {
        let root = std::env::temp_dir().join(format!("kureha-reload-ui-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("file.rs");
        std::fs::write(&path, "fn first() {}\n").unwrap();
        let app = App::new();
        let id = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .open(path.clone(), "fn first() {}\n");
        let bindings = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .bindings
            .clone();
        std::fs::write(&path, "fn second() {}\n").unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        loop {
            app.poll_external_files();
            if bindings.document.get().as_str() == "fn second() {}\n" {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "external reload was not published"
            );
            thread::sleep(Duration::from_millis(10));
        }
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
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn external_reload_preserves_local_edits_and_rejects_stale_results() {
        let mut workspace = Workspace::default();
        let id = workspace.open("external.rs".into(), "struct A {}\n");
        let doc = workspace.get(id).unwrap();
        let update = Update {
            snapshot: Snapshot {
                id,
                revision: doc.editor.revision(),
                path: doc.path.clone().unwrap(),
                disk: doc.disk_text.clone().unwrap(),
                stamp: None,
            },
            stamp: Stamp {
                modified: SystemTime::now(),
                len: 14,
            },
            text: Some("\r\nstruct A {}\r\n".into()),
        };
        assert_eq!(apply(&mut workspace, &update), Some(true));
        let doc = workspace.get(id).unwrap();
        assert_eq!(doc.editor.document.as_str(), "\nstruct A {}\n");
        assert!(doc.crlf);
        assert!(!doc.editor.dirty());
        assert!(doc.editor.revision() > update.snapshot.revision);
        assert_eq!(apply(&mut workspace, &update), None);
        let doc = workspace.get_mut(id).unwrap();
        let mut current = update;
        current.snapshot.revision = doc.editor.revision();
        current.snapshot.disk = doc.disk_text.clone().unwrap();
        doc.editor.edit_transaction(&EditDelta {
            replaced_range: 0..0,
            inserted: "// mine\n".into(),
        });
        current.snapshot.revision = doc.editor.revision();
        assert_eq!(apply(&mut workspace, &current), None);
        assert!(
            workspace
                .get(id)
                .unwrap()
                .editor
                .document
                .as_str()
                .starts_with("// mine")
        );
        workspace.remove(id);
        assert_eq!(apply(&mut workspace, &current), None);
    }

    #[test]
    fn disk_scan_detects_replacement_and_keeps_deleted_documents() {
        let root = std::env::temp_dir().join(format!("kureha-reload-{}", std::process::id()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("file.rs");
        std::fs::write(&path, "first").unwrap();
        let mut snapshot = Snapshot {
            id: 1,
            revision: 0,
            path: path.clone(),
            disk: Arc::from("first"),
            stamp: None,
        };
        let initial = scan(vec![snapshot.clone()]).pop().unwrap();
        assert!(initial.text.is_none());
        snapshot.stamp = Some(initial.stamp);
        assert!(scan(vec![snapshot.clone()]).is_empty());
        files::save(&path, "second revision", false).unwrap();
        assert_eq!(
            scan(vec![snapshot.clone()])[0].text.as_deref(),
            Some("second revision")
        );
        std::fs::remove_file(&path).unwrap();
        assert!(scan(vec![snapshot]).is_empty());
        std::fs::remove_dir(root).unwrap();
    }
}
