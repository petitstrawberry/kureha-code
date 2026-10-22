//! Open documents have stable identities independent of their tab positions.
use crate::editor::Editor;
use scarlet_ui::generate_state_id;
use scarlet_ui::prelude::*;
use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
    },
};

pub type DocumentId = u64;

#[derive(Clone)]
pub struct Bindings {
    pub document: State<TextDocument>,
    pub selection: State<TextSelection>,
    pub scroll: State<TextViewScroll>,
    generation: Arc<AtomicU64>,
}

impl Bindings {
    fn new(editor: &Editor) -> Self {
        let document = State::new(generate_state_id(), editor.document.clone());
        let selection = State::new(generate_state_id(), editor.selection);
        let generation = Arc::new(AtomicU64::new(0));
        // Notifications only touch an atomic: publishing UI state never takes
        // the workspace mutex, including updates made by TextView itself.
        for state in [&document as &dyn Listenable, &selection as &dyn Listenable] {
            let generation = generation.clone();
            state.subscribe_any(Arc::new(move || {
                generation.fetch_add(1, Ordering::Relaxed);
            }));
        }
        Self {
            document,
            selection,
            scroll: State::initial(generate_state_id()),
            generation,
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Relaxed)
    }
}

pub struct Document {
    pub id: DocumentId,
    pub path: Option<PathBuf>,
    pub crlf: bool,
    pub editor: Editor,
    pub bindings: Bindings,
}

impl Document {
    fn new(id: DocumentId, path: Option<PathBuf>, text: &str) -> Self {
        let editor = Editor::new(text);
        let bindings = Bindings::new(&editor);
        Self {
            id,
            path,
            crlf: text.contains("\r\n"),
            editor,
            bindings,
        }
    }
    pub fn name(&self) -> String {
        self.path
            .as_ref()
            .and_then(|path| path.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_else(|| format!("Untitled-{}", self.id))
    }
}

pub struct Workspace {
    pub documents: Vec<Document>,
    next_id: DocumentId,
}

impl Default for Workspace {
    fn default() -> Self {
        let mut workspace = Self {
            documents: Vec::new(),
            next_id: 1,
        };
        workspace.new_document();
        workspace
    }
}

impl Workspace {
    pub fn get(&self, id: DocumentId) -> Option<&Document> {
        self.documents.iter().find(|doc| doc.id == id)
    }
    pub fn get_mut(&mut self, id: DocumentId) -> Option<&mut Document> {
        self.documents.iter_mut().find(|doc| doc.id == id)
    }
    pub fn index(&self, id: DocumentId) -> Option<usize> {
        self.documents.iter().position(|doc| doc.id == id)
    }
    pub fn find_path(&self, path: &Path) -> Option<DocumentId> {
        self.documents
            .iter()
            .find(|doc| doc.path.as_deref() == Some(path))
            .map(|doc| doc.id)
    }
    pub fn new_document(&mut self) -> DocumentId {
        self.add(None, "")
    }
    pub fn open(&mut self, path: PathBuf, text: &str) -> DocumentId {
        if let Some(id) = self.find_path(&path) {
            return id;
        }
        self.add(Some(path), text)
    }
    fn add(&mut self, path: Option<PathBuf>, text: &str) -> DocumentId {
        let id = self.next_id;
        self.next_id += 1;
        self.documents.push(Document::new(id, path, text));
        id
    }
    pub fn remove(&mut self, id: DocumentId) {
        self.documents.retain(|doc| doc.id != id);
        if self.documents.is_empty() {
            self.new_document();
        }
    }
    /// A save completion belongs to its original tab, never whichever tab is
    /// active now. Edits made after the snapshot stay dirty.
    pub fn saved(&mut self, id: DocumentId, revision: u64, path: PathBuf) -> bool {
        let Some(doc) = self.get_mut(id) else {
            return false;
        };
        doc.path = Some(path);
        if doc.editor.revision() == revision {
            doc.editor.saved();
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn documents_keep_independent_history_caret_scroll_and_dirty_state() {
        let mut workspace = Workspace::default();
        let first = workspace.documents[0].id;
        let second = workspace.open("other.rs".into(), "other");
        let doc = workspace.get_mut(first).unwrap();
        doc.editor.edit(&EditDelta {
            replaced_range: 0..0,
            inserted: "猫".into(),
        });
        doc.bindings.selection.set(TextSelection::collapsed(3));
        doc.bindings.scroll.set(TextViewScroll { x: 12.0, y: 70.0 });
        assert!(!workspace.get(second).unwrap().editor.dirty());
        assert_eq!(
            workspace
                .get(second)
                .unwrap()
                .bindings
                .selection
                .get()
                .caret
                .byte,
            0
        );
        let doc = workspace.get_mut(first).unwrap();
        assert_eq!(doc.bindings.scroll.get().y, 70.0);
        assert_eq!(doc.bindings.selection.get().caret.byte, 3);
        doc.editor.undo(false);
        assert_eq!(doc.editor.document.as_str(), "");
        assert_eq!(
            workspace.get(second).unwrap().editor.document.as_str(),
            "other"
        );
    }
    #[test]
    fn save_completion_cannot_clean_newer_edits_or_a_reused_tab_position() {
        let mut workspace = Workspace::default();
        let first = workspace.documents[0].id;
        let doc = workspace.get_mut(first).unwrap();
        doc.editor.edit(&EditDelta {
            replaced_range: 0..0,
            inserted: "a".into(),
        });
        let revision = doc.editor.revision();
        doc.editor.edit(&EditDelta {
            replaced_range: 1..1,
            inserted: "b".into(),
        });
        assert!(workspace.saved(first, revision, "first.rs".into()));
        assert!(workspace.get(first).unwrap().editor.dirty());
        workspace.remove(first);
        let replacement = workspace.documents[0].id;
        assert_ne!(first, replacement);
        assert!(!workspace.saved(first, revision, "wrong.rs".into()));
        assert!(workspace.get(replacement).unwrap().path.is_none());
    }
    #[test]
    fn reopening_an_open_path_preserves_unsaved_edits() {
        let mut workspace = Workspace::default();
        let id = workspace.open("file.rs".into(), "old");
        workspace.get_mut(id).unwrap().editor.edit(&EditDelta {
            replaced_range: 0..3,
            inserted: "unsaved".into(),
        });
        assert_eq!(workspace.open("file.rs".into(), "disk"), id);
        assert_eq!(
            workspace.get(id).unwrap().editor.document.as_str(),
            "unsaved"
        );
    }
}
