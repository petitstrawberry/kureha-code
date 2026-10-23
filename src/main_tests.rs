use kureha_code::editor::Editor;

use super::*;
use scarlet_ui::clipboard::{ClipboardError, ClipboardProvider};
use std::time::{Duration, Instant};

#[test]
fn shifted_split_shortcut_accepts_logical_pipe_character() {
    use scarlet_ui::pipeline::RenderingPipeline;
    let app = test_app("first\nsecond", None, false);
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_paint_enabled(false);
    pipeline.set_root(
        Window::new("Editor", app.content())
            .decorated(false)
            .size(Size::new(1100.0, 760.0))
            .create_element(),
    );
    pipeline.layout_initial();
    pipeline.render();
    let mut modifiers = scarlet_ui::event::KeyModifiers::empty();
    if cfg!(target_os = "macos") {
        modifiers.super_key = true;
    } else {
        modifiers.control = true;
    }
    modifiers.shift = true;
    pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
        keycode: KeyCode::Char('|'),
        modifiers,
    }));
    assert!(app.split_down.get());
    assert_eq!(app.second.get(), Some(app.group_id(0)));
    pipeline.teardown();
}

#[test]
fn split_editors_share_text_and_undo_but_keep_tabs_selection_and_scroll_independent() {
    let app = test_app("first\nsecond\nthird", Some("RIGHT"), false);
    let id = app.active_id();
    let left = app.bindings(id).unwrap();
    left.selection.set(TextSelection::collapsed(3));
    left.scroll.set(TextViewScroll { x: 10.0, y: 42.0 });
    app.split_editor(false);
    let right = app.bindings(id).unwrap();
    right.selection.set(TextSelection {
        anchor: TextPosition::new(6),
        caret: TextPosition::new(12),
    });
    right.scroll.set(TextViewScroll { x: 0.0, y: 90.0 });
    app.paste();
    finish_clipboard(&app);
    assert_eq!(left.document.get().as_str(), "first\nRIGHT\nthird");
    assert_eq!(left.selection.get(), TextSelection::collapsed(3));
    assert_eq!(left.scroll.get().y, 42.0);
    assert_eq!(right.scroll.get().y, 90.0);
    app.undo(false);
    assert_eq!(left.document.get().as_str(), "first\nsecond\nthird");
    assert_eq!(right.selection.get().normalized_range(), Some(6..12));
    app.new_document();
    let other = app.active_id();
    assert_ne!(other, id);
    assert_eq!(app.group_id(0), id);
    assert_eq!(app.group_id(1), other);
    app.activate(id);
    app.select_all();
    app.paste();
    finish_clipboard(&app);
    app.request_close(Some(id));
    assert!(app.runtime.lock().unwrap().close.is_none());
    assert!(
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .editor
            .dirty()
    );
    assert_eq!(app.group_id(0), id);
    app.close_split();
    assert!(app.second.get().is_none());
    assert!(app.runtime.lock().unwrap().group_tabs[0].contains(&other));
}

struct TestClipboard {
    text: Arc<Mutex<Option<String>>>,
    fail_write: bool,
}

#[test]
fn save_all_writes_dirty_buffers_and_stops_before_overwriting_external_edits() {
    let root = std::env::temp_dir().join(format!("kureha-save-all-{}", std::process::id()));
    std::fs::create_dir_all(&root).unwrap();
    let app = App::new();
    let mut ids = Vec::new();
    for name in ["a.rs", "b.rs", "c.rs"] {
        let path = root.join(name);
        std::fs::write(&path, "before\n").unwrap();
        let id = app.runtime.lock().unwrap().workspace.open(path, "before\n");
        app.activate(id);
        let doc = app.bindings(id).unwrap();
        doc.selection.set(TextSelection::collapsed(0));
        app.edit_command(super::editing_ui::Command::Indent);
        ids.push(id);
    }
    std::fs::write(root.join("b.rs"), "external\n").unwrap();
    app.save_all();
    let until = Instant::now() + Duration::from_secs(5);
    while app.runtime.lock().unwrap().job.is_some() {
        app.poll();
        assert!(Instant::now() < until);
        thread::yield_now();
    }
    assert_eq!(
        std::fs::read_to_string(root.join("a.rs")).unwrap(),
        "    before\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("b.rs")).unwrap(),
        "external\n"
    );
    assert_eq!(
        std::fs::read_to_string(root.join("c.rs")).unwrap(),
        "before\n"
    );
    assert!(app.status.get().contains("changed on disk"));
    assert!(app.runtime.lock().unwrap().save_all.is_empty());
    std::fs::write(root.join("b.rs"), "before\n").unwrap();
    app.save_all();
    while app.runtime.lock().unwrap().job.is_some() {
        app.poll();
        assert!(Instant::now() < until);
        thread::yield_now();
    }
    for id in ids {
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
    assert_eq!(
        std::fs::read_to_string(root.join("c.rs")).unwrap(),
        "    before\n"
    );
    std::fs::remove_dir_all(root).unwrap();
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
        move || Ok(Completion::Save(PathBuf::from("saved.txt"), "first".into())),
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
        Ok(Completion::Save(PathBuf::from("first.txt"), "first".into())),
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
        Ok(Completion::Save("first.txt".into(), "first".into())),
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

#[test]
fn explorer_keeps_scroll_and_user_width_when_workbench_updates() {
    use scarlet_ui::event::{MouseButton, MouseEvent, ScrollSource, WheelPhase};
    use scarlet_ui::pipeline::RenderingPipeline;
    let app = App::new();
    app.runtime.lock().unwrap().explorer.root = Some(PathBuf::from("/project"));
    app.tree_rows.set(
        (0..100)
            .map(|i| Row {
                key: format!("file-{i}"),
                path: Some(PathBuf::from(format!("/project/sample-{i:03}.rs"))),
                label: format!("sample-{i:03}.rs"),
                depth: 0,
                directory: false,
                expanded: false,
            })
            .collect(),
    );
    let window = || {
        Window::new("Editor", app.content())
            .decorated(false)
            .size(Size::new(1100.0, 760.0))
    };
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_paint_enabled(false);
    pipeline.set_root(window().create_element());
    pipeline.layout_initial();
    pipeline.render();
    fn sidebar(
        e: &dyn Element,
        origin: Point,
    ) -> Option<(scarlet_ui::element::ElementId, f32, f32)> {
        let p = Point::new(origin.x + e.position().x, origin.y + e.position().y);
        if e.type_name_debug().contains("ScrollViewRenderObject")
            && p.x < 100.0
            && e.bounds().size.height > 200.0
        {
            return Some((e.id(), e.bounds().size.width, e.children()[0].position().y));
        }
        e.children()
            .iter()
            .find_map(|child| sidebar(child.as_ref(), p))
    }
    let state = |p: &RenderingPipeline| {
        sidebar(p.element_tree().root().unwrap(), Point::ZERO).expect("explorer scroll view")
    };
    for event in [
        MouseEvent::ButtonPressed {
            button: MouseButton::Left,
            x: 245,
            y: 350,
            click_count: 1,
        },
        MouseEvent::Moved { x: 330, y: 350 },
        MouseEvent::ButtonReleased {
            button: MouseButton::Left,
            x: 330,
            y: 350,
            click_count: 1,
        },
    ] {
        pipeline.handle_event(&Event::Mouse(event));
        pipeline.render();
        if matches!(event, MouseEvent::Moved { .. }) {
            pipeline
                .element_tree_mut()
                .root_mut()
                .unwrap()
                .update(&window());
            pipeline.layout_initial();
            pipeline.render();
        }
    }
    let resized = state(&pipeline);
    assert!(resized.1 > 300.0, "sidebar drag: {resized:?}");
    pipeline.handle_event(&Event::Mouse(MouseEvent::Moved { x: 400, y: 350 }));
    pipeline.render();
    assert_eq!(
        state(&pipeline),
        resized,
        "released divider kept following the pointer"
    );
    pipeline.handle_event(&Event::Mouse(MouseEvent::Wheel {
        delta_x: 0,
        delta_y: -243,
        x: 100,
        y: 300,
        phase: WheelPhase::Moved,
        source: ScrollSource::Trackpad,
    }));
    pipeline.render();
    let scrolled = state(&pipeline);
    assert!(scrolled.2 < -30.0, "sidebar wheel: {scrolled:?}");
    for index in 0..3 {
        app.position.set(format!("Ln {}, Col 1", index + 2));
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.layout_initial();
        pipeline.render();
        assert_eq!(
            state(&pipeline),
            scrolled,
            "workbench rebuild reset user state"
        );
    }
    let x = scrolled.1 as i32 + 3;
    pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
        button: MouseButton::Left,
        x,
        y: 350,
        click_count: 1,
    }));
    pipeline.handle_event(&Event::Mouse(MouseEvent::Moved { x: x + 25, y: 350 }));
    pipeline.render();
    let before_blur = state(&pipeline);
    pipeline.handle_event(&Event::Focus(scarlet_ui::event::FocusEvent::Lost));
    pipeline.handle_event(&Event::Mouse(MouseEvent::Moved { x: x + 100, y: 350 }));
    pipeline.render();
    assert_eq!(
        state(&pipeline),
        before_blur,
        "focus loss left the divider dragging"
    );
    pipeline.teardown();
}

#[test]
fn selection_and_find_overlay_preserve_document_viewport_and_scroll() {
    use scarlet_ui::event::{MouseButton, MouseEvent, ScrollSource, WheelPhase};
    use scarlet_ui::pipeline::RenderingPipeline;
    let app = test_app(
        &("fn example() { let value = 123; }\n".repeat(250)),
        None,
        false,
    );
    let window = || {
        Window::new("Editor", app.content())
            .decorated(false)
            .size(Size::new(1100.0, 760.0))
    };
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_paint_enabled(false);
    pipeline.set_root(window().create_element());
    pipeline.layout_initial();
    pipeline.render();
    fn document(e: &dyn Element, origin: Point) -> Option<(scarlet_ui::element::ElementId, Rect)> {
        let p = Point::new(origin.x + e.position().x, origin.y + e.position().y);
        if e.type_name_debug().contains("TextViewRenderObject") {
            return Some((e.id(), Rect::new(p, e.bounds().size)));
        }
        e.children().iter().find_map(|c| document(c.as_ref(), p))
    }
    let original = document(pipeline.element_tree().root().unwrap(), Point::ZERO).unwrap();
    let x = (original.1.origin.x + 110.0) as i32;
    let y = (original.1.origin.y + 80.0) as i32;
    pipeline.handle_event(&Event::Mouse(MouseEvent::Wheel {
        delta_x: 0,
        delta_y: -3000,
        x,
        y,
        phase: WheelPhase::Moved,
        source: ScrollSource::Trackpad,
    }));
    pipeline.render();
    let bindings = app.bindings(app.active_id()).unwrap();
    let scroll = bindings.scroll.get();
    assert!(scroll.y > 1000.0);
    pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
        button: MouseButton::Left,
        x,
        y,
        click_count: 1,
    }));
    let anchor = bindings.selection.get().anchor;
    for offset in [20, 45, 70, 45, 20] {
        pipeline.handle_event(&Event::Mouse(MouseEvent::Moved {
            x: x + offset,
            y: y + offset,
        }));
        app.position.set(format!(
            "Ln {}, Col {}",
            bindings.selection.get().caret.byte,
            offset
        ));
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.layout_initial();
        pipeline.render();
        assert_eq!(
            document(pipeline.element_tree().root().unwrap(), Point::ZERO).unwrap(),
            original
        );
        assert_eq!(
            bindings.scroll.get(),
            scroll,
            "selection rebuild moved the document"
        );
        assert_eq!(bindings.selection.get().anchor, anchor);
    }
    pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
        button: MouseButton::Left,
        x,
        y,
        click_count: 1,
    }));
    for open in [true, false, true] {
        app.find_open.set(open);
        pipeline
            .element_tree_mut()
            .root_mut()
            .unwrap()
            .update(&window());
        pipeline.layout_initial();
        pipeline.render();
        assert_eq!(
            document(pipeline.element_tree().root().unwrap(), Point::ZERO).unwrap(),
            original,
            "Find changed document height or identity"
        );
        assert_eq!(bindings.scroll.get(), scroll);
    }
    app.find_open.set(false);
    pipeline
        .element_tree_mut()
        .root_mut()
        .unwrap()
        .update(&window());
    pipeline.layout_initial();
    pipeline.render();
    let mut primary = scarlet_ui::event::KeyModifiers::empty();
    if cfg!(target_os = "macos") {
        primary.super_key = true;
    } else {
        primary.control = true;
    }
    pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
        keycode: KeyCode::Char('a'),
        modifiers: primary,
    }));
    pipeline.render();
    assert_eq!(
        bindings.selection.get().normalized_range(),
        Some(0..bindings.document.get().len()),
        "closing Find did not return keyboard focus to the document"
    );
    pipeline.teardown();
}

#[test]
fn changing_theme_updates_existing_button_text_and_editor_surfaces() {
    use scarlet_ui::element::LayoutConstraints;
    use scarlet_ui::renderer::{PaintCommand, PaintContext};
    let app = test_app("fn main() {}", None, false);
    let mut root = app.content().create_element();
    root.layout(LayoutConstraints::tight(1100.0, 760.0));
    app.set_theme(kureha_code::theme::Theme::Dark);
    root.update(&app.content());
    root.layout(LayoutConstraints::tight(1100.0, 760.0));
    fn inspect(e: &dyn Element, expected: Color, found: &mut bool) {
        if let Some(render) = e.render_object() {
            let mut ctx = PaintContext::new();
            render.paint(&mut ctx, Point::ZERO);
            for c in ctx.commands() {
                if let PaintCommand::DrawText { text, color, .. } = c {
                    if text == "Open Folder" {
                        assert_eq!(*color, expected);
                        *found = true;
                    }
                }
            }
        }
        for child in e.children() {
            inspect(child.as_ref(), expected, found);
        }
    }
    let mut found = false;
    inspect(
        root.as_ref(),
        kureha_code::theme::Theme::Dark.colors().foreground,
        &mut found,
    );
    assert!(found);
}

#[test]
fn clicking_document_returns_shortcuts_from_terminal_to_editor() {
    use scarlet_ui::event::{MouseButton, MouseEvent};
    use scarlet_ui::pipeline::RenderingPipeline;
    let app = test_app("fn main() {}", None, false);
    app.terminal_visible.set(true);
    app.terminal.focus();
    let window = || {
        Window::new("Focus", app.content())
            .decorated(false)
            .size(Size::new(1100.0, 760.0))
    };
    let mut pipeline = RenderingPipeline::new();
    pipeline.set_paint_enabled(false);
    pipeline.set_root(window().create_element());
    pipeline.layout_initial();
    pipeline.render();
    pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonPressed {
        button: MouseButton::Left,
        x: 500,
        y: 160,
        click_count: 1,
    }));
    pipeline.handle_event(&Event::Mouse(MouseEvent::ButtonReleased {
        button: MouseButton::Left,
        x: 500,
        y: 160,
        click_count: 1,
    }));
    pipeline.render();
    assert!(
        !app.terminal.focused(),
        "terminal kept focus after document click"
    );
    let mut modifiers = scarlet_ui::event::KeyModifiers::empty();
    if cfg!(target_os = "macos") {
        modifiers.super_key = true;
    } else {
        modifiers.control = true;
    }
    pipeline.handle_event(&Event::Keyboard(KeyEvent::Pressed {
        keycode: KeyCode::Char('f'),
        modifiers,
    }));
    pipeline.render();
    assert!(
        app.find_open.get(),
        "Find shortcut did not reach document after leaving terminal"
    );
    pipeline.teardown();
}

fn lifecycle_document(app: &App, name: &str) -> DocumentId {
    let id = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .open(PathBuf::from(name), "first\nsecond\nthird");
    app.activate(id);
    id
}

fn finish_lifecycle_open(app: &App) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while app.runtime.lock().unwrap().job.is_some() {
        app.poll();
        assert!(Instant::now() < deadline, "file open did not complete");
        thread::yield_now();
    }
}

#[test]
fn split_lifecycle_remove_right_tab_preserves_left_and_uses_right_neighbors() {
    for close_active in [false, true] {
        let app = test_app("first\nsecond\nthird", None, false);
        let left = app.active_id();
        let left_view = app.bindings_for(left, 0).unwrap();
        left_view.selection.set(TextSelection::collapsed(2));
        app.split_editor(false);
        let background = lifecycle_document(&app, "background.txt");
        let current = lifecycle_document(&app, "current.txt");
        let next = lifecycle_document(&app, "next.txt");
        app.activate(current);
        let current_view = app.bindings_for(current, 1).unwrap();
        current_view.selection.set(TextSelection::collapsed(7));
        let next_view = app.bindings_for(next, 1).unwrap();
        next_view.selection.set(TextSelection::collapsed(9));
        let closed = if close_active { current } else { background };

        app.request_close(Some(closed));

        assert_eq!(app.group_id(0), left);
        assert_eq!(app.group_id(1), if close_active { next } else { current });
        assert_eq!(app.focused_group.get(), 1);
        assert_eq!(left_view.selection.get(), TextSelection::collapsed(2));
        assert_eq!(next_view.selection.get(), TextSelection::collapsed(9));
        if !close_active {
            assert_eq!(current_view.selection.get(), TextSelection::collapsed(7));
        }
        let rt = app.runtime.lock().unwrap();
        assert!(rt.workspace.get(closed).is_none());
        assert_eq!(rt.group_tabs[0], vec![left]);
        assert!(rt.group_tabs[1].contains(&app.second.get().unwrap()));
    }
}

#[test]
fn split_lifecycle_remove_left_tab_never_selects_a_right_only_document() {
    let app = test_app("first\nsecond\nthird", None, false);
    let left = app.active_id();
    let closed = lifecycle_document(&app, "left-only.txt");
    app.activate(left);
    app.split_editor(false);
    let right = lifecycle_document(&app, "right-only.txt");
    let right_view = app.bindings_for(right, 1).unwrap();
    right_view.selection.set(TextSelection::collapsed(8));
    app.focus_group(0);
    app.activate(closed);

    app.request_close(Some(closed));

    assert_eq!(app.group_id(0), left);
    assert_eq!(app.group_id(1), right);
    assert_eq!(app.focused_group.get(), 0);
    assert_eq!(right_view.selection.get(), TextSelection::collapsed(8));
    let rt = app.runtime.lock().unwrap();
    assert_eq!(rt.group_tabs[0], vec![left]);
    assert_eq!(rt.group_tabs[1], vec![left, right]);
}

#[test]
fn split_lifecycle_removing_last_group_tab_keeps_the_other_group() {
    for group in [0, 1] {
        let app = test_app("first\nsecond\nthird", None, false);
        let left = app.active_id();
        app.split_editor(false);
        let right = lifecycle_document(&app, "right-only.txt");
        app.request_close(Some(left));
        let right_view = app.bindings_for(right, 1).unwrap();
        right_view.selection.set(TextSelection::collapsed(8));
        app.focus_group(group);

        app.request_close(Some(if group == 0 { left } else { right }));

        if group == 0 {
            assert_ne!(app.group_id(0), right);
            assert_ne!(app.group_id(0), left);
            assert_eq!(app.group_id(1), right);
            assert_eq!(right_view.selection.get(), TextSelection::collapsed(8));
            assert_eq!(app.runtime.lock().unwrap().group_tabs[1], vec![right]);
        } else {
            assert_eq!(app.group_id(0), left);
            assert!(app.second.get().is_none());
            assert!(app.runtime.lock().unwrap().group_tabs[1].is_empty());
        }
        assert_eq!(app.focused_group.get(), 0);
    }
}

#[test]
fn split_lifecycle_closing_background_shared_tab_keeps_active_selection() {
    let app = test_app("first\nsecond\nthird", None, false);
    let shared = app.active_id();
    let other = lifecycle_document(&app, "other.txt");
    let current = lifecycle_document(&app, "current.txt");
    app.activate(shared);
    app.split_editor(false);
    let right_view = app.bindings_for(shared, 1).unwrap();
    right_view.selection.set(TextSelection::collapsed(8));
    app.focus_group(0);
    app.activate(current);
    let current_view = app.bindings_for(current, 0).unwrap();
    current_view.selection.set(TextSelection::collapsed(3));

    app.request_close(Some(shared));

    assert_eq!(app.group_id(0), current);
    assert_eq!(app.group_id(1), shared);
    assert_eq!(app.focused_group.get(), 0);
    assert_eq!(current_view.selection.get(), TextSelection::collapsed(3));
    assert_eq!(right_view.selection.get(), TextSelection::collapsed(8));
    let rt = app.runtime.lock().unwrap();
    assert!(rt.workspace.get(shared).is_some());
    assert_eq!(rt.group_tabs[0], vec![other, current]);
    assert_eq!(rt.group_tabs[1], vec![shared]);
}

#[test]
fn split_lifecycle_async_open_stays_in_origin_without_stealing_focus() {
    let path =
        std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main_tests.rs"))
            .unwrap();
    for origin in [0, 1] {
        let app = test_app("first\nsecond\nthird", None, false);
        let left = app.active_id();
        app.split_editor(false);
        let right = lifecycle_document(&app, "right-only.txt");
        let left_view = app.bindings_for(left, 0).unwrap();
        let right_view = app.bindings_for(right, 1).unwrap();
        left_view.selection.set(TextSelection::collapsed(2));
        right_view.selection.set(TextSelection::collapsed(8));
        app.focus_group(origin);
        app.open(path.clone());
        assert!(matches!(
            app.runtime.lock().unwrap().job.as_ref().map(|job| &job.operation),
            Some(FileOperation::Open { group }) if *group == origin
        ));
        app.focus_group(1 - origin);
        let focused = app.active_id();

        finish_lifecycle_open(&app);

        let opened = app
            .runtime
            .lock()
            .unwrap()
            .workspace
            .find_path(&path)
            .unwrap();
        assert_eq!(app.group_id(origin), opened);
        assert_eq!(app.focused_group.get(), 1 - origin);
        assert_eq!(app.active_id(), focused);
        assert_eq!(left_view.selection.get(), TextSelection::collapsed(2));
        assert_eq!(right_view.selection.get(), TextSelection::collapsed(8));
        let rt = app.runtime.lock().unwrap();
        assert!(rt.group_tabs[origin].contains(&opened));
        assert!(!rt.group_tabs[1 - origin].contains(&opened));
    }
}

#[test]
fn split_lifecycle_async_open_after_group_close_is_a_background_tab() {
    let path =
        std::fs::canonicalize(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/main_tests.rs"))
            .unwrap();
    let app = test_app("first\nsecond\nthird", None, false);
    let left = app.active_id();
    let left_view = app.bindings_for(left, 0).unwrap();
    left_view.selection.set(TextSelection::collapsed(3));
    app.split_editor(false);
    app.open(path.clone());
    app.close_split();

    finish_lifecycle_open(&app);

    let rt = app.runtime.lock().unwrap();
    let opened = rt.workspace.find_path(&path).unwrap();
    assert!(rt.group_tabs[0].contains(&opened));
    assert!(rt.group_tabs[1].is_empty());
    drop(rt);
    assert!(app.second.get().is_none());
    assert_eq!(app.focused_group.get(), 0);
    assert_eq!(app.active_id(), left);
    assert_eq!(left_view.selection.get(), TextSelection::collapsed(3));
}

#[test]
fn split_selections_follow_paste_indentation_and_undo_redo() {
    let app = test_app("abc\ndef", Some("XX"), false);
    let id = app.active_id();
    let left = app.bindings(id).unwrap();
    left.selection.set(TextSelection {
        anchor: TextPosition::new(1),
        caret: TextPosition::new(2),
    });
    app.split_editor(false);
    let right = app.bindings(id).unwrap();
    right.selection.set(TextSelection::collapsed(0));
    app.paste();
    finish_clipboard(&app);
    assert_eq!(left.selection.get().normalized_range(), Some(3..4));
    app.undo(false);
    assert_eq!(left.selection.get().normalized_range(), Some(1..2));
    app.undo(true);
    assert_eq!(left.selection.get().normalized_range(), Some(3..4));
    app.undo(false);
    right.selection.set(TextSelection {
        anchor: TextPosition::new(0),
        caret: TextPosition::new(7),
    });
    app.edit_command(super::editing_ui::Command::Indent);
    assert_eq!(left.document.get().as_str(), "    abc\n    def");
    assert_eq!(left.selection.get().normalized_range(), Some(5..6));
    app.undo(false);
    assert_eq!(left.selection.get().normalized_range(), Some(1..2));
    app.undo(true);
    assert_eq!(left.selection.get().normalized_range(), Some(5..6));
}
