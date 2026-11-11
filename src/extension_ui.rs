//! Runs extension commands on the extension worker and applies their results.
use super::App;
use kureha_code::{
    extensions::{
        host::{self, Reply},
        runtime::DocumentSnapshot,
    },
    workspace::DocumentId,
};
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, Mutex, atomic::Ordering},
};

/// What a queued command needs to apply its reply safely later.
struct Pending {
    document: DocumentId,
    revision: u64,
    navigation: u64,
    text: Arc<String>,
    label: String,
}

// Request ids come from one process-wide worker, so one map serves every window.
static PENDING: Mutex<Option<HashMap<u64, Pending>>> = Mutex::new(None);

impl App {
    pub(super) fn run_extension_command(&self, extension: &str, command: &str) {
        if self.blocked() {
            return;
        }
        let Some(host) = host::host() else {
            self.status.set("Extensions are still loading".into());
            return;
        };
        let label = kureha_code::extensions::loaded()
            .and_then(|registry| {
                let extension = registry
                    .extensions
                    .iter()
                    .find(|e| e.manifest.id == extension)?;
                let title = &extension
                    .manifest
                    .contributes
                    .commands
                    .iter()
                    .find(|c| c.id == command)?
                    .title;
                Some(format!("{}: {title}", extension.manifest.name))
            })
            .unwrap_or_else(|| command.to_owned());
        let Some(id) = self.active_id() else {
            self.status.set(format!("{label} needs an open editor"));
            return;
        };
        let Some(bindings) = self.bindings(id) else {
            return;
        };
        let selection = bindings.selection.get();
        let (start, end) = (
            selection.anchor.byte.min(selection.caret.byte),
            selection.anchor.byte.max(selection.caret.byte),
        );
        let (path, revision, text) = {
            let rt = self.runtime.lock().unwrap();
            let Some(doc) = rt.workspace.get(id) else {
                return;
            };
            (
                doc.path.clone().unwrap_or_else(PathBuf::new),
                doc.editor.revision(),
                Arc::new(doc.editor.document.as_str().to_string()),
            )
        };
        let snapshot = DocumentSnapshot {
            path,
            version: revision,
            selection: start.min(text.len())..end.min(text.len()),
            text: text.clone(),
        };
        let navigation = self.navigation.load(Ordering::Relaxed);
        match host.command(extension, command, snapshot) {
            Some(request) => {
                PENDING
                    .lock()
                    .unwrap()
                    .get_or_insert_with(HashMap::new)
                    .insert(
                        request,
                        Pending {
                            document: id,
                            revision,
                            navigation,
                            text,
                            label: label.clone(),
                        },
                    );
                self.status.set(format!("{label}…"));
            }
            None => self.status.set("The extension worker has stopped".into()),
        }
    }

    pub(super) fn poll_extensions(&self) {
        let Some(host) = host::running() else {
            return;
        };
        for reply in host.poll() {
            let Reply::Command { id, result } = reply else {
                continue;
            };
            let Some(pending) = PENDING
                .lock()
                .unwrap()
                .as_mut()
                .and_then(|map| map.remove(&id))
            else {
                continue;
            };
            match result {
                Err(error) => self.status.set(error),
                Ok(outcome) if outcome.edits.is_empty() => self.status.set(
                    outcome
                        .status
                        .unwrap_or_else(|| format!("{} finished", pending.label)),
                ),
                Ok(outcome) => {
                    let edits = outcome
                        .edits
                        .into_iter()
                        .map(|edit| (edit.range, edit.text))
                        .collect::<Vec<_>>();
                    let applied = self.apply_offset_edits(
                        pending.document,
                        pending.revision,
                        pending.navigation,
                        &pending.text,
                        &edits,
                        &pending.label,
                    );
                    // The extension's message replaces "applied", never "discarded".
                    if let Some(status) = outcome.status.filter(|_| applied) {
                        self.status.set(status);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::EditDelta;

    fn app_with(text: &str) -> (App, DocumentId) {
        let app = App::with_document();
        let id = app.document_id();
        app.edit(
            id,
            &EditDelta {
                replaced_range: 0..0,
                inserted: text.into(),
            },
        );
        (app, id)
    }
    fn revision(app: &App, id: DocumentId) -> u64 {
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .editor
            .revision()
    }

    #[test]
    fn extension_edits_apply_as_one_undo_step() {
        let text = "名前 = 1";
        let (app, id) = app_with(text);
        let applied = app.apply_offset_edits(
            id,
            revision(&app, id),
            app.navigation.load(Ordering::Relaxed),
            text,
            &[(0..6, "name".into()), (9..10, "2".into())],
            "Demo: Rename",
        );
        assert!(applied);
        assert_eq!(
            app.bindings(id).unwrap().document.get().as_str(),
            "name = 2"
        );
        app.undo(false);
        assert_eq!(app.bindings(id).unwrap().document.get().as_str(), text);
    }

    #[test]
    fn extension_edits_for_an_older_revision_are_discarded() {
        let (app, id) = app_with("abcd");
        let stale = revision(&app, id);
        app.edit(
            id,
            &EditDelta {
                replaced_range: 4..4,
                inserted: "e".into(),
            },
        );
        let applied = app.apply_offset_edits(
            id,
            stale,
            app.navigation.load(Ordering::Relaxed),
            "abcd",
            &[(0..4, "zzzz".into())],
            "Demo: Upper",
        );
        assert!(!applied);
        assert_eq!(app.bindings(id).unwrap().document.get().as_str(), "abcde");
        assert!(app.status.get().contains("discarded"));
    }
}
