//! The upstream editing engine owns revisions and undo. ScarletUI receives only
//! the changed range, keeping the platform and its widgets out of the engine.
use floem_editor_core::{
    buffer::{Buffer, rope_text::RopeText},
    cursor::CursorMode,
    editor::EditType,
    selection::Selection,
    xi_rope::RopeDelta,
};
use scarlet_ui::prelude::{EditDelta, TextDocument, TextPosition, TextSelection};

pub struct Editor {
    buffer: Buffer,
    pub document: TextDocument,
    pub selection: TextSelection,
}

impl Default for Editor {
    fn default() -> Self {
        Self::new("")
    }
}

impl Editor {
    pub fn new(text: &str) -> Self {
        // Internal LF offsets stay identical between the UI and the engine.
        // The file layer retains CRLF on disk.
        let text = text.replace("\r\n", "\n").replace('\r', "\n");
        let mut buffer = Buffer::new(text.as_str());
        buffer.set_pristine();
        Self {
            document: TextDocument::from_str(&text),
            buffer,
            selection: TextSelection::default(),
        }
    }

    pub fn dirty(&self) -> bool {
        !self.buffer.is_pristine()
    }
    pub fn revision(&self) -> u64 {
        self.buffer.rev()
    }
    pub fn saved(&mut self) {
        self.buffer.set_pristine();
        self.buffer.reset_edit_type();
    }

    pub fn moved(&mut self, selection: TextSelection) {
        if selection != self.selection {
            self.buffer.reset_edit_type();
        }
        self.selection = selection;
    }

    pub fn edit(&mut self, edit: &EditDelta) {
        let before = self.selection;
        let kind = if edit.inserted.contains(['\n', '\r']) {
            EditType::InsertNewline
        } else if edit.is_insertion() {
            EditType::InsertChars
        } else if edit.is_deletion() {
            EditType::Delete
        } else {
            EditType::Other
        };
        let selection = Selection::region(edit.replaced_range.start, edit.replaced_range.end);
        let (_, delta, _) = self
            .buffer
            .edit([(selection, edit.inserted.as_str())], kind);
        self.buffer
            .set_cursor_before(CursorMode::Insert(to_engine(before)));
        let (range, length) = delta.summary();
        self.selection = TextSelection::collapsed(range.start() + length);
        self.buffer
            .set_cursor_after(CursorMode::Insert(to_engine(self.selection)));
        self.apply(&delta);
    }

    /// Apply one command as an undo step separate from edits on either side.
    /// Ordinary typing and deletion still coalesce through `edit`.
    pub fn edit_transaction(&mut self, edit: &EditDelta) {
        self.buffer.reset_edit_type();
        self.edit(edit);
        self.buffer.reset_edit_type();
    }

    pub fn undo(&mut self, redo: bool) {
        let result = if redo {
            self.buffer.do_redo()
        } else {
            self.buffer.do_undo()
        };
        if let Some((_, delta, _, cursor)) = result {
            self.apply(&delta);
            if let Some(CursorMode::Insert(selection)) = cursor {
                if let Some(region) = selection.first() {
                    self.selection = TextSelection {
                        anchor: TextPosition::new(region.start),
                        caret: TextPosition::new(region.end),
                    };
                }
            }
        }
    }

    fn apply(&mut self, delta: &RopeDelta) {
        let (range, length) = delta.summary();
        let inserted = self
            .buffer
            .slice_to_cow(range.start()..range.start() + length);
        self.document = self
            .document
            .replace(range.start()..range.end(), &inserted)
            .0;
    }
}

fn to_engine(selection: TextSelection) -> Selection {
    Selection::region(selection.anchor.byte, selection.caret.byte)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_snapshot(editor: &Editor, text: &str, selection: TextSelection) {
        assert_eq!(editor.document.as_str(), text);
        assert_eq!(editor.buffer.to_string(), text);
        assert_eq!(editor.selection, selection);
    }

    #[test]
    fn paste_is_one_undo_step_between_typing_groups() {
        let mut editor = Editor::default();
        for text in ["a", "b"] {
            let byte = editor.selection.caret.byte;
            editor.edit(&EditDelta {
                replaced_range: byte..byte,
                inserted: text.into(),
            });
        }
        editor.edit_transaction(&EditDelta {
            replaced_range: 2..2,
            inserted: "猫🍁".into(),
        });
        for text in ["c", "d"] {
            let byte = editor.selection.caret.byte;
            editor.edit(&EditDelta {
                replaced_range: byte..byte,
                inserted: text.into(),
            });
        }
        assert_snapshot(
            &editor,
            "ab猫🍁cd",
            TextSelection::collapsed("ab猫🍁cd".len()),
        );
        for text in ["ab猫🍁", "ab", ""] {
            editor.undo(false);
            assert_snapshot(&editor, text, TextSelection::collapsed(text.len()));
        }
        assert!(!editor.dirty());
        for text in ["ab", "ab猫🍁", "ab猫🍁cd"] {
            editor.undo(true);
            assert_snapshot(&editor, text, TextSelection::collapsed(text.len()));
        }
        assert!(editor.dirty());
    }

    #[test]
    fn cut_is_one_undo_step_between_deletions_and_restores_reversed_selection() {
        let mut editor = Editor::new("a猫🍁xy");
        editor.moved(TextSelection::collapsed(10));
        editor.edit(&EditDelta {
            replaced_range: 9..10,
            inserted: String::new(),
        });
        let cut_selection = TextSelection {
            anchor: TextPosition::new(8),
            caret: TextPosition::new(1),
        };
        editor.moved(cut_selection);
        editor.edit_transaction(&EditDelta {
            replaced_range: 1..8,
            inserted: String::new(),
        });
        assert_snapshot(&editor, "ax", TextSelection::collapsed(1));
        for range in [1..2, 0..1] {
            editor.edit(&EditDelta {
                replaced_range: range,
                inserted: String::new(),
            });
        }
        assert_snapshot(&editor, "", TextSelection::collapsed(0));
        for (text, selection) in [
            ("ax", TextSelection::collapsed(1)),
            ("a猫🍁x", cut_selection),
            ("a猫🍁xy", TextSelection::collapsed(10)),
        ] {
            editor.undo(false);
            assert_snapshot(&editor, text, selection);
        }
        assert!(!editor.dirty());
        for (text, byte) in [("a猫🍁x", 9), ("ax", 1), ("", 0)] {
            editor.undo(true);
            assert_snapshot(&editor, text, TextSelection::collapsed(byte));
        }
        assert!(editor.dirty());
    }

    #[test]
    fn unicode_replacement_undo_redo_and_saved_revision() {
        let mut editor = Editor::new("let 名前 = 1;\r\n");
        editor.moved(TextSelection {
            anchor: TextPosition::new(4),
            caret: TextPosition::new(10),
        });
        editor.edit(&EditDelta {
            replaced_range: 4..10,
            inserted: "value🍁".into(),
        });
        assert_eq!(editor.document.as_str(), "let value🍁 = 1;\n");
        assert!(editor.dirty());
        editor.saved();
        editor.undo(false);
        assert_eq!(editor.document.as_str(), "let 名前 = 1;\n");
        assert_eq!(editor.selection.anchor.byte, 4);
        assert_eq!(editor.selection.caret.byte, 10);
        assert!(editor.dirty());
        editor.undo(true);
        assert_eq!(editor.document.as_str(), "let value🍁 = 1;\n");
        assert!(!editor.dirty());
        assert_eq!(editor.document.as_str(), editor.buffer.to_string());
    }
}
