use kureha_code::editor::Editor;

use super::*;
use scarlet_ui::clipboard::{ClipboardError, ClipboardProvider};
use std::time::{Duration, Instant};

struct TestClipboard {
    text: Arc<Mutex<Option<String>>>,
    fail_write: bool,
}

impl ClipboardProvider for TestClipboard {
    fn read_text(&self) -> std::result::Result<Option<String>, ClipboardError> {
        Ok(self.text.lock().unwrap().clone())
    }

    fn write_text(&self, text: &str) -> std::result::Result<(), ClipboardError> {
        if self.fail_write {
            return Err(ClipboardError::AccessDenied);
        }
        *self.text.lock().unwrap() = Some(text.into());
        Ok(())
    }
}

fn test_app(text: &str, clipboard_text: Option<&str>, fail_write: bool) -> App {
    let app = App::new();
    {
        let mut rt = app.runtime.lock().unwrap();
        rt.workspace.documents[0].editor = Editor::new(text);
        rt.clipboard = Some(TextClipboard::new(TestClipboard {
            text: Arc::new(Mutex::new(clipboard_text.map(str::to_owned))),
            fail_write,
        }));
    }
    app.sync_editor(app.active_id());
    app
}

fn finish_clipboard(app: &App) {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        app.poll_clipboard();
        if app.runtime.lock().unwrap().clipboard_job.is_none() {
            return;
        }
        assert!(Instant::now() < deadline, "clipboard did not complete");
        thread::yield_now();
    }
}

#[test]
fn copy_and_cut_preserve_unicode_and_cut_is_undoable() {
    let app = test_app("a猫🍁z", None, false);
    let selection = TextSelection {
        anchor: TextPosition::new(8),
        caret: TextPosition::new(1),
    };
    app.bindings(app.active_id())
        .unwrap()
        .selection
        .set(selection);
    app.copy(false);
    finish_clipboard(&app);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "a猫🍁z"
    );
    assert_eq!(
        app.bindings(app.active_id()).unwrap().selection.get(),
        selection
    );
    assert!(
        !app.runtime.lock().unwrap().workspace.documents[0]
            .editor
            .dirty()
    );
    app.copy(true);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "a猫🍁z"
    );
    finish_clipboard(&app);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "az"
    );
    assert_eq!(
        app.bindings(app.active_id()).unwrap().selection.get(),
        TextSelection::collapsed(1)
    );
    app.undo(false);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "a猫🍁z"
    );
    assert_eq!(
        app.bindings(app.active_id()).unwrap().selection.get(),
        selection
    );
    app.undo(true);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "az"
    );
    app.paste();
    finish_clipboard(&app);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "a猫🍁z"
    );
}

#[test]
fn failed_or_oversized_cut_never_deletes_text() {
    for text in [
        "猫 text".into(),
        "x".repeat(scarlet_ui::clipboard::MAX_TEXT_BYTES + 1),
    ] {
        let app = test_app(&text, Some("original clipboard"), true);
        app.select_all();
        let selection = app.bindings(app.active_id()).unwrap().selection.get();
        app.copy(true);
        finish_clipboard(&app);
        assert_eq!(
            app.bindings(app.active_id())
                .unwrap()
                .document
                .get()
                .as_str(),
            text
        );
        assert_eq!(
            app.bindings(app.active_id()).unwrap().selection.get(),
            selection
        );
        assert!(
            !app.runtime.lock().unwrap().workspace.documents[0]
                .editor
                .dirty()
        );
        assert!(app.status.get().contains("Focus") || app.status.get().contains("256 KiB"));
    }
}

#[test]
fn paste_normalizes_line_endings_and_restores_selection_on_undo() {
    let app = test_app("replace me", Some("猫\r\n🍁\rfin"), false);
    app.select_all();
    let selection = app.bindings(app.active_id()).unwrap().selection.get();
    app.paste();
    finish_clipboard(&app);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "猫\n🍁\nfin"
    );
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .selection
            .get()
            .caret
            .byte,
        "猫\n🍁\nfin".len()
    );
    app.undo(false);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "replace me"
    );
    assert_eq!(
        app.bindings(app.active_id()).unwrap().selection.get(),
        selection
    );
}

#[test]
fn clipboard_paste_undo_is_separate_from_adjacent_typing() {
    let app = test_app("", Some("猫🍁"), false);
    app.edit(
        app.active_id(),
        &EditDelta {
            replaced_range: 0..0,
            inserted: "before".into(),
        },
    );
    app.paste();
    finish_clipboard(&app);
    let byte = app
        .bindings(app.active_id())
        .unwrap()
        .selection
        .get()
        .caret
        .byte;
    app.edit(
        app.active_id(),
        &EditDelta {
            replaced_range: byte..byte,
            inserted: "after".into(),
        },
    );
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "before猫🍁after"
    );
    for text in ["before猫🍁", "before", ""] {
        app.undo(false);
        assert_eq!(
            app.bindings(app.active_id())
                .unwrap()
                .document
                .get()
                .as_str(),
            text
        );
        assert_eq!(
            app.bindings(app.active_id()).unwrap().selection.get(),
            TextSelection::collapsed(text.len())
        );
    }
    for text in ["before", "before猫🍁", "before猫🍁after"] {
        app.undo(true);
        assert_eq!(
            app.bindings(app.active_id())
                .unwrap()
                .document
                .get()
                .as_str(),
            text
        );
    }
}

#[test]
fn empty_clipboard_and_empty_copy_leave_selection_and_history_alone() {
    for text in [None, Some("")] {
        let app = test_app("keep", text, false);
        app.copy(true);
        assert!(app.runtime.lock().unwrap().clipboard_job.is_none());
        app.select_all();
        let selection = app.bindings(app.active_id()).unwrap().selection.get();
        app.paste();
        finish_clipboard(&app);
        assert_eq!(
            app.bindings(app.active_id())
                .unwrap()
                .document
                .get()
                .as_str(),
            "keep"
        );
        assert_eq!(
            app.bindings(app.active_id()).unwrap().selection.get(),
            selection
        );
        assert!(
            !app.runtime.lock().unwrap().workspace.documents[0]
                .editor
                .dirty()
        );
        assert!(app.status.get().contains("empty"));
    }
}

#[test]
fn cut_and_paste_reject_changed_or_restored_targets() {
    let changes: [fn(&App); 4] = [
        |app| {
            app.bindings(app.active_id())
                .unwrap()
                .selection
                .set(TextSelection::collapsed(0))
        },
        |app| {
            let selection = app.bindings(app.active_id()).unwrap().selection.get();
            app.bindings(app.active_id())
                .unwrap()
                .selection
                .set(TextSelection::collapsed(0));
            app.bindings(app.active_id())
                .unwrap()
                .selection
                .set(selection);
        },
        |app| {
            app.edit(
                app.active_id(),
                &EditDelta {
                    replaced_range: 0..5,
                    inserted: "typed".into(),
                },
            );
            app.undo(false);
        },
        |app| {
            let mut rt = app.runtime.lock().unwrap();
            let revision = rt.workspace.documents[0].editor.revision();
            rt.workspace.documents[0].editor = Editor::new("other");
            assert_eq!(rt.workspace.documents[0].editor.revision(), revision);
            drop(rt);
            app.sync_editor(app.active_id());
            app.select_all();
        },
    ];
    for cut in [false, true] {
        for change in changes {
            let app = test_app("first", Some("replacement"), false);
            app.select_all();
            if cut {
                app.copy(true);
            } else {
                app.paste();
            }
            assert!(
                !!app.editable(app.active_id()),
                "clipboard must not freeze typing"
            );
            change(&app);
            let before = app.bindings(app.active_id()).unwrap().document.get();
            let selection = app.bindings(app.active_id()).unwrap().selection.get();
            finish_clipboard(&app);
            assert_eq!(
                app.bindings(app.active_id())
                    .unwrap()
                    .document
                    .get()
                    .as_str(),
                before.as_str()
            );
            assert_eq!(
                app.bindings(app.active_id()).unwrap().selection.get(),
                selection
            );
            assert!(app.status.get().contains("cancelled"));
        }
    }
}

#[test]
fn file_jobs_block_new_commands_and_drain_stale_clipboard_results() {
    for cut in [false, true] {
        let app = test_app("first", Some("replacement"), false);
        app.select_all();
        if cut {
            app.copy(true);
        } else {
            app.paste();
        }
        let (sender, receiver) = mpsc::channel();
        let id = app.active_id();
        app.runtime.lock().unwrap().job = Some(FileJob {
            operation: FileOperation::Save { id, revision: 0 },
            receiver,
        });
        finish_clipboard(&app);
        assert_eq!(
            app.bindings(app.active_id())
                .unwrap()
                .document
                .get()
                .as_str(),
            "first"
        );
        assert!(app.status.get().contains("cancelled"));
        app.copy(true);
        app.paste();
        assert!(app.runtime.lock().unwrap().clipboard_job.is_none());
        drop(sender);
    }

    // Even a file job that finishes before the clipboard must invalidate
    // its target; comparing only the current busy flag would miss this.
    let app = test_app("first", Some("replacement"), false);
    app.select_all();
    app.paste();
    let revision = app.runtime.lock().unwrap().workspace.documents[0]
        .editor
        .revision();
    app.worker(
        FileOperation::Save {
            id: app.active_id(),
            revision,
        },
        move || Ok(Completion::Save(PathBuf::from("saved.txt"))),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while !app.editable(app.active_id()) {
        app.poll();
        assert!(Instant::now() < deadline, "file job did not complete");
        thread::yield_now();
    }
    finish_clipboard(&app);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "first"
    );
    assert!(app.status.get().contains("cancelled"));
}

#[test]
fn menu_commands_are_queued_and_execute_on_poll() {
    use scarlet_ui::menu_model::{
        invoke_menu_callback, register_menu_callbacks, unregister_menu_callbacks,
    };
    let app = test_app("猫🍁", None, false);
    let window_id = u32::MAX;
    register_menu_callbacks(window_id, &app.menu());
    for id in ["select-all", "copy", "cut", "paste"] {
        let before = app.bindings(app.active_id()).unwrap().document.get();
        let selection = app.bindings(app.active_id()).unwrap().selection.get();
        assert!(invoke_menu_callback(window_id, id));
        let deadline = Instant::now() + Duration::from_secs(3);
        while app.runtime.lock().unwrap().menu_actions.is_empty() {
            assert!(Instant::now() < deadline, "menu callback did not complete");
            thread::yield_now();
        }
        assert_eq!(
            app.bindings(app.active_id())
                .unwrap()
                .document
                .get()
                .as_str(),
            before.as_str()
        );
        assert_eq!(
            app.bindings(app.active_id()).unwrap().selection.get(),
            selection
        );
        assert!(app.runtime.lock().unwrap().clipboard_job.is_none());
        app.poll_menu();
        finish_clipboard(&app);
    }
    unregister_menu_callbacks(window_id);
    assert_eq!(
        app.bindings(app.active_id())
            .unwrap()
            .document
            .get()
            .as_str(),
        "猫🍁"
    );
}

#[test]
fn new_tabs_keep_unsaved_text_and_each_tabs_caret_scroll_and_undo() {
    let app = test_app("", None, false);
    let first = app.active_id();
    app.edit(
        first,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "first猫".into(),
        },
    );
    let bindings = app.bindings(first).unwrap();
    bindings.selection.set(TextSelection::collapsed(5));
    bindings.scroll.set(TextViewScroll { x: 12.0, y: 90.0 });
    app.new_document();
    let second = app.active_id();
    assert_ne!(first, second);
    app.edit(
        second,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "second".into(),
        },
    );
    app.activate(first);
    assert_eq!(bindings.document.get().as_str(), "first猫");
    assert_eq!(bindings.selection.get().caret.byte, 5);
    assert_eq!(bindings.scroll.get(), TextViewScroll { x: 12.0, y: 90.0 });
    app.undo(false);
    assert_eq!(bindings.document.get().as_str(), "");
    assert_eq!(
        app.bindings(second).unwrap().document.get().as_str(),
        "second"
    );
}

#[test]
fn clipboard_is_cancelled_after_switching_tabs_even_when_switching_back() {
    for cut in [false, true] {
        let app = test_app("first", Some("clipboard"), false);
        let first = app.active_id();
        app.select_all();
        if cut {
            app.copy(true);
        } else {
            app.paste();
        }
        app.new_document();
        let second = app.active_id();
        app.activate(first);
        finish_clipboard(&app);
        assert_eq!(
            app.bindings(first).unwrap().document.get().as_str(),
            "first"
        );
        assert_eq!(app.bindings(second).unwrap().document.get().as_str(), "");
        assert!(app.status.get().contains("cancelled"));
    }
}

#[test]
fn close_cancel_and_window_cancel_preserve_dirty_tabs_including_discard_decisions() {
    let app = test_app("", None, false);
    let first = app.active_id();
    app.edit(
        first,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "one".into(),
        },
    );
    app.new_document();
    let second = app.active_id();
    app.edit(
        second,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "two".into(),
        },
    );
    app.request_close(Some(first));
    assert_eq!(
        app.runtime.lock().unwrap().close.as_ref().unwrap().target,
        Some(first)
    );
    app.new_document();
    assert_eq!(app.runtime.lock().unwrap().workspace.documents.len(), 2);
    app.cancel_close();
    assert_eq!(app.bindings(first).unwrap().document.get().as_str(), "one");
    app.request_close(None);
    app.discard_close();
    assert_eq!(
        app.runtime.lock().unwrap().close.as_ref().unwrap().target,
        Some(second)
    );
    app.cancel_close();
    let rt = app.runtime.lock().unwrap();
    assert_eq!(rt.workspace.documents.len(), 2);
    assert!(rt.workspace.get(first).unwrap().editor.dirty());
    assert!(rt.workspace.get(second).unwrap().editor.dirty());
}

#[test]
fn discard_closes_only_target_and_save_completion_updates_original_tab() {
    let app = test_app("", None, false);
    let first = app.active_id();
    app.edit(
        first,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "one".into(),
        },
    );
    let revision = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .get(first)
        .unwrap()
        .editor
        .revision();
    app.new_document();
    let second = app.active_id();
    app.edit(
        second,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "two".into(),
        },
    );
    app.complete_file(
        FileOperation::Save {
            id: first,
            revision,
        },
        Ok(Completion::Save(PathBuf::from("first.txt"))),
    );
    assert_eq!(app.active_id(), second);
    {
        let rt = app.runtime.lock().unwrap();
        assert!(!rt.workspace.get(first).unwrap().editor.dirty());
        assert!(rt.workspace.get(second).unwrap().editor.dirty());
        assert!(rt.workspace.get(second).unwrap().path.is_none());
    }
    app.request_close(Some(second));
    app.discard_close();
    assert!(app.bindings(second).is_none());
    assert_eq!(app.active_id(), first);
    assert_eq!(app.bindings(first).unwrap().document.get().as_str(), "one");
}

#[test]
fn saving_freezes_only_its_target_and_cannot_clean_newer_revision() {
    let app = test_app("", None, false);
    let first = app.active_id();
    app.edit(
        first,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "old".into(),
        },
    );
    let revision = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .get(first)
        .unwrap()
        .editor
        .revision();
    app.new_document();
    let second = app.active_id();
    let (_sender, receiver) = mpsc::channel();
    app.runtime.lock().unwrap().job = Some(FileJob {
        operation: FileOperation::Save {
            id: first,
            revision,
        },
        receiver,
    });
    assert!(!app.editable(first));
    assert!(app.editable(second));
    app.edit(
        second,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "other tab".into(),
        },
    );
    app.runtime.lock().unwrap().job = None;
    app.edit(
        first,
        &EditDelta {
            replaced_range: 3..3,
            inserted: "new".into(),
        },
    );
    app.complete_file(
        FileOperation::Save {
            id: first,
            revision,
        },
        Ok(Completion::Save("first.txt".into())),
    );
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
    assert_eq!(
        app.bindings(second).unwrap().document.get().as_str(),
        "other tab"
    );
}

#[test]
fn find_replace_all_is_one_transaction_and_empty_query_does_not_edit() {
    let app = test_app("猫 and 猫", None, false);
    let id = app.active_id();
    app.query.set("猫".into());
    app.find(false);
    assert_eq!(
        app.bindings(id).unwrap().selection.get().normalized_range(),
        Some(0..3)
    );
    app.replacement.set("🍁".into());
    app.replace_all();
    assert_eq!(
        app.bindings(id).unwrap().document.get().as_str(),
        "🍁 and 🍁"
    );
    app.undo(false);
    assert_eq!(
        app.bindings(id).unwrap().document.get().as_str(),
        "猫 and 猫"
    );
    app.query.set(String::new());
    app.replace_all();
    assert_eq!(
        app.bindings(id).unwrap().document.get().as_str(),
        "猫 and 猫"
    );
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
fn workbench_lays_out_without_a_window_at_normal_and_minimum_sizes() {
    use scarlet_ui::element::LayoutConstraints;
    let app = test_app("fn main() {}\n", None, false);
    app.new_document();
    let generation = app.runtime.lock().unwrap().explorer.begin_root_request();
    app.runtime.lock().unwrap().explorer.set_root(
        generation,
        "/project".into(),
        Listing {
            entries: vec![explorer::Entry {
                path: "/project/src".into(),
                directory: true,
            }],
            truncated: false,
        },
    );
    app.refresh_tree();
    for find in [false, true] {
        app.find_open.set(find);
        for size in [Size::new(1100.0, 760.0), Size::new(720.0, 400.0)] {
            let mut element = app.content().create_element();
            assert_eq!(
                element.layout(LayoutConstraints::tight(size.width, size.height)),
                size
            );
            fn inspect(element: &dyn Element, editors: &mut usize) {
                let bounds = element.bounds();
                assert!(
                    bounds.size.width.is_finite() && bounds.size.height.is_finite(),
                    "{} has invalid bounds",
                    element.type_name()
                );
                if element.type_name_debug().contains("TextViewRenderObject") {
                    *editors += 1;
                    assert!(bounds.size.width >= 400.0 && bounds.size.height > 160.0);
                }
                for child in element.children() {
                    inspect(child.as_ref(), editors);
                }
            }
            let mut editors = 0;
            inspect(element.as_ref(), &mut editors);
            assert!(editors > 0, "workbench must contain an editor");
        }
    }
}
