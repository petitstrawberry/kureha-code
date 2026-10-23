use super::*;
#[test]
fn utf16_positions_keep_japanese_and_astral_characters_on_boundaries() {
    let text = "a猫🍁b\n末尾";
    for byte in (0..=text.len()).filter(|&n| text.is_char_boundary(n)) {
        assert_eq!(offset(text, &position(text, byte)), Some(byte));
    }
    assert_eq!(offset(text, &json!({"line":0,"character":3})), None);
    assert_eq!(offset(text, &json!({"line":9,"character":0})), None);
}
#[test]
fn completion_is_one_undo_step_and_applies_additional_edits() {
    let app = App::new();
    let id = app.active_id();
    {
        let mut rt = app.runtime.lock().unwrap();
        let doc = rt.workspace.get_mut(id).unwrap();
        doc.editor.edit_transaction(&EditDelta {
            replaced_range: 0..0,
            inserted: "// 猫\nfn main() { fo }".into(),
        });
        let text = doc.editor.document.as_str().to_string();
        let end = text.find("fo }").unwrap() + 2;
        let selection = TextSelection::collapsed(end);
        doc.editor.moved(selection);
        doc.bindings.selection.set(selection);
        let guard = Guard {
            id,
            revision: doc.editor.revision(),
            caret: end,
            navigation: app.navigation.load(Ordering::Relaxed),
        };
        let edit = json!({"label":"foo", "textEdit":{"range":{"start":position(&text,end-2),"end":position(&text,end)},"newText":"foo()"},"additionalTextEdits":[{"range":{"start":position(&text,0),"end":position(&text,0)},"newText":"use thing::foo;\n"}]});
        rt.language.panel = Panel::Completion(
            guard,
            vec![Choice {
                label: "foo".into(),
                detail: String::new(),
                item: edit,
            }],
        );
    }
    app.accept_completion(0);
    let rt = app.runtime.lock().unwrap();
    let doc = rt.workspace.get(id).unwrap();
    assert_eq!(
        doc.editor.document.as_str(),
        "use thing::foo;\n// 猫\nfn main() { foo() }"
    );
    drop(rt);
    app.undo(false);
    assert_eq!(
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .editor
            .document
            .as_str(),
        "// 猫\nfn main() { fo }"
    );
}
#[test]
fn stale_completion_does_not_replace_newer_text() {
    let app = App::new();
    let id = app.active_id();
    {
        let mut rt = app.runtime.lock().unwrap();
        let doc = rt.workspace.get_mut(id).unwrap();
        let guard = Guard {
            id,
            revision: doc.editor.revision(),
            caret: 0,
            navigation: 0,
        };
        doc.editor.edit_transaction(&EditDelta {
            replaced_range: 0..0,
            inserted: "new".into(),
        });
        rt.language.panel = Panel::Completion(
            guard,
            vec![Choice {
                label: "old".into(),
                detail: String::new(),
                item: json!({"label":"old"}),
            }],
        );
    }
    app.accept_completion(0);
    assert_eq!(
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .get(id)
            .unwrap()
            .editor
            .document
            .as_str(),
        "new"
    );
}

#[test]
fn cancelled_background_diagnostics_keep_the_user_status() {
    let app = App::new();
    let id = app.active_id();
    let snapshot = Snapshot {
        id,
        path: PathBuf::from("/tmp/main.rs"),
        revision: 0,
        text: Arc::new(String::new()),
        caret: 0,
        saved: true,
    };
    app.status.set("Saved main.rs".into());
    let pending = |method: &str| Pending {
        guard: Guard {
            id,
            revision: 0,
            caret: 0,
            navigation: 0,
        },
        method: method.into(),
        params: Value::Null,
        hover_generation: None,
        document_version: None,
        attempts: 0,
        started: Instant::now(),
    };
    app.runtime.lock().unwrap().language.sessions.insert(
        "fixture".into(),
        Session {
            client: None,
            starting: None,
            initialize: None,
            ready: true,
            capabilities: Value::Null,
            pending: BTreeMap::from([
                (1, pending("textDocument/diagnostic")),
                (2, pending("textDocument/hover")),
            ]),
            documents: BTreeMap::new(),
            root: PathBuf::from("/tmp"),
            language: "rust".into(),
            label: "fixture".into(),
            failure: None,
            started: Instant::now(),
            diagnostic_requests: BTreeMap::new(),
        },
    );
    assert!(!app.language_event(
        "fixture",
        LspEvent::Response {
            id: 1,
            result: Value::Null,
            error: Some(lsp::ResponseError {
                code: -32802,
                message: "server cancelled while indexing".into(),
                data: Some(json!({"retriggerRequest":true})),
            }),
        },
        &[snapshot.clone()]
    ));
    assert_eq!(app.status.get(), "Saved main.rs");
    assert!(app.language_event(
        "fixture",
        LspEvent::Response {
            id: 2,
            result: Value::Null,
            error: Some(lsp::ResponseError {
                code: -32603,
                message: "Hover unavailable".into(),
                data: None,
            }),
        },
        &[snapshot]
    ));
    assert_eq!(app.status.get(), "Language server: Hover unavailable");
}

#[test]
#[ignore = "requires installed rust-analyzer; run explicitly for integration validation"]
fn real_rust_analyzer_reaches_app_definition_completion_and_diagnostics() {
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let fixture =
        Fixture(std::env::temp_dir().join(format!("kureha-language-app-{}", std::process::id())));
    std::fs::create_dir(&fixture.0).unwrap();
    std::fs::create_dir(fixture.0.join("src")).unwrap();
    std::fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = \"kureha_language_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let source = "pub fn greet() -> i32 { 7 }\nfn main() { let _value = greet(); missing_name(); gre }\nuse missing_crate::Absent;\nfn broken( {\n";
    let path = fixture.0.join("src/main.rs");
    std::fs::write(&path, source).unwrap();
    let app = App::new();
    let id = {
        let mut rt = app.runtime.lock().unwrap();
        rt.explorer.root = Some(fixture.0.clone());
        rt.workspace.open(path.clone(), source)
    };
    app.activate(id);
    let deadline = Instant::now() + Duration::from_secs(45);
    loop {
        app.poll_language();
        let ready = app
            .runtime
            .lock()
            .unwrap()
            .language
            .sessions
            .values()
            .any(|s| s.ready);
        assert!(
            Instant::now() < deadline,
            "server initialization failed: {}",
            app.language_status.get()
        );
        if ready {
            break;
        }
        thread::sleep(Duration::from_millis(30));
    }
    let call = source.find("greet();").unwrap() + 2;
    {
        let mut rt = app.runtime.lock().unwrap();
        let doc = rt.workspace.get_mut(id).unwrap();
        let selection = TextSelection::collapsed(call);
        doc.bindings.selection.set(selection);
        doc.editor.moved(selection);
    }
    let mut last = Instant::now() - Duration::from_secs(5);
    loop {
        app.poll_language();
        if app.bindings(id).unwrap().selection.get().start().byte == 7 {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "definition failed: {}",
            app.status.get()
        );
        if last.elapsed() > Duration::from_secs(2) {
            app.language_request("textDocument/definition");
            last = Instant::now();
        }
        thread::sleep(Duration::from_millis(30));
    }
    eprintln!("APP_DEFINITION_OK {}", path.display());
    let caret = source.rfind("gre }").unwrap() + 3;
    {
        let mut rt = app.runtime.lock().unwrap();
        let doc = rt.workspace.get_mut(id).unwrap();
        let selection = TextSelection::collapsed(caret);
        doc.bindings.selection.set(selection);
        doc.editor.moved(selection);
    }
    app.language_request("textDocument/completion");
    let selected = loop {
        app.poll_language();
        let selected = {
            let rt = app.runtime.lock().unwrap();
            match &rt.language.panel {
                Panel::Completion(_, items) => {
                    items.iter().position(|c| c.label.starts_with("greet"))
                }
                _ => None,
            }
        };
        if let Some(index) = selected {
            break index;
        }
        assert!(
            Instant::now() < deadline,
            "completion failed: {}",
            app.status.get()
        );
        thread::sleep(Duration::from_millis(30));
    };
    let expected = {
        let rt = app.runtime.lock().unwrap();
        let Panel::Completion(_, items) = &rt.language.panel else {
            panic!("missing completion panel")
        };
        let item = &items[selected].item;
        eprintln!("APP_COMPLETION_ITEM {item}");
        item.pointer("/textEdit/newText")
            .or_else(|| item.get("insertText"))
            .and_then(Value::as_str)
            .unwrap_or("greet")
            .to_string()
    };
    app.accept_completion(selected);
    let text = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .get(id)
        .unwrap()
        .editor
        .document
        .as_str()
        .to_string();
    assert!(
        text.contains(&format!("{expected} }}\n")),
        "completion result: {text:?}"
    );
    eprintln!("APP_COMPLETION_APPLY_OK");
    loop {
        app.poll_language();
        if !app.language_diagnostics(id).is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "diagnostics absent: {}",
            app.language_status.get()
        );
        thread::sleep(Duration::from_millis(30));
    }
    assert!(!app.language_diagnostics(id).is_empty());
    eprintln!("APP_DIAGNOSTICS_OK {}", app.language_status.get());
}

#[test]
fn completion_keys_choose_and_insert_without_adding_a_tab_or_newline() {
    let app = App::new();
    let id = app.active_id();
    app.edit(
        id,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "gre".into(),
        },
    );
    {
        let mut rt = app.runtime.lock().unwrap();
        let doc = rt.workspace.get(id).unwrap();
        let guard = Guard {
            id,
            revision: doc.editor.revision(),
            caret: 3,
            navigation: app.navigation.load(Ordering::Relaxed),
        };
        rt.language.panel = Panel::Completion(
            guard,
            ["green", "greet"]
                .into_iter()
                .map(|label| Choice {
                    label: label.into(),
                    detail: String::new(),
                    item: json!({"label":label}),
                })
                .collect(),
        );
    }
    assert!(app.language_key(KeyCode::Down, Default::default()));
    assert!(app.language_key(KeyCode::Tab, Default::default()));
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), "greet");
    app.undo(false);
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), "gre");
}

#[test]
fn pointer_hover_keeps_the_caret_and_invalidates_old_pointer_targets() {
    let app = App::new();
    let id = app.active_id();
    app.edit(
        id,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "fn greet() {}".into(),
        },
    );
    let selection = app.bindings(id).unwrap().selection.get();
    app.pointer_hover(id, Some((4, Point::new(80.0, 40.0))));
    let generation = app.runtime.lock().unwrap().language.hover_generation;
    assert_eq!(app.bindings(id).unwrap().selection.get(), selection);
    app.pointer_hover(id, None);
    let rt = app.runtime.lock().unwrap();
    assert!(rt.language.hover_target.is_none());
    assert!(rt.language.hover_generation > generation);
}

fn edit_value(text: &str, bytes: std::ops::Range<usize>, replacement: &str) -> Value {
    json!({"range":{"start":position(text, bytes.start),"end":position(text, bytes.end)},"newText":replacement})
}
fn current_guard(app: &App) -> Guard {
    let id = app.active_id();
    let caret = app.bindings(id).unwrap().selection.get().caret.byte;
    let revision = app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .get(id)
        .unwrap()
        .editor
        .revision();
    Guard {
        id,
        revision,
        caret,
        navigation: app.navigation.load(Ordering::Relaxed),
    }
}
#[test]
fn formatting_and_rename_are_one_undo_and_keep_redo_selection() {
    let app = App::new();
    let id = app.active_id();
    let original = "fn main(){let 名前=2;println!(\"{}\",名前);}";
    app.edit(
        id,
        &EditDelta {
            replaced_range: 0..0,
            inserted: original.into(),
        },
    );
    let selection = TextSelection {
        anchor: TextPosition::new(original.find("名前").unwrap() + "名前".len()),
        caret: TextPosition::new(original.find("名前").unwrap()),
    };
    app.bindings(id).unwrap().selection.set(selection);
    let edits: Vec<_> = original
        .match_indices("名前")
        .map(|(start, _)| edit_value(original, start..start + "名前".len(), "renamed"))
        .collect();
    app.apply_language_edits(&current_guard(&app), &edits, "Rename");
    let after = app.bindings(id).unwrap().selection.get();
    assert_eq!(
        app.bindings(id).unwrap().document.get().as_str(),
        original.replace("名前", "renamed")
    );
    app.undo(false);
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), original);
    assert_eq!(app.bindings(id).unwrap().selection.get(), selection);
    app.undo(true);
    assert_eq!(app.bindings(id).unwrap().selection.get(), after);
    let before = app
        .bindings(id)
        .unwrap()
        .document
        .get()
        .as_str()
        .to_string();
    let formatted = "fn main() {\n    let renamed = 2;\n    println!(\"{}\", renamed);\n}\n";
    app.apply_language_edits(
        &current_guard(&app),
        &[edit_value(&before, 0..before.len(), formatted)],
        "Format",
    );
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), formatted);
    app.undo(false);
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), before);
}
#[test]
fn language_edits_reject_changed_document_and_overlaps_without_partial_application() {
    let app = App::new();
    let id = app.active_id();
    app.edit(
        id,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "abcd".into(),
        },
    );
    let guard = current_guard(&app);
    app.edit(
        id,
        &EditDelta {
            replaced_range: 4..4,
            inserted: "e".into(),
        },
    );
    app.apply_language_edits(&guard, &[edit_value("abcd", 0..4, "old")], "Format");
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), "abcde");
    app.apply_language_edits(
        &current_guard(&app),
        &[
            edit_value("abcde", 0..3, "one"),
            edit_value("abcde", 2..4, "two"),
        ],
        "Rename",
    );
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), "abcde");
    assert!(app.status.get().contains("Overlapping"));
    let invalid = json!({"range":{"start":{"line":0,"character":1},"end":{"line":2,"character":0}},"newText":"bad"});
    assert!(prepare_edits("猫", &[invalid], TextSelection::collapsed(0)).is_err());
}
#[test]
fn rename_rejects_all_cross_file_resource_operations_and_wrong_versions() {
    let path = Path::new("/tmp/main.rs");
    let edit = edit_value("value", 0..5, "next");
    let same = json!({"documentChanges":[{"textDocument":{"uri":uri(path),"version":7},"edits":[edit.clone()]}]});
    assert_eq!(rename_edits(&same, path, Some(7)).unwrap().len(), 1);
    assert!(
        rename_edits(&same, path, Some(8))
            .unwrap_err()
            .contains("version")
    );
    let cross =
        json!({"changes":{uri(path):[edit.clone()],"file:///tmp/unopened.rs":[edit.clone()]}});
    assert!(
        rename_edits(&cross, path, Some(7))
            .unwrap_err()
            .contains("other files")
    );
    let resource = json!({"documentChanges":[{"textDocument":{"uri":uri(path),"version":7},"edits":[edit]},{"kind":"rename","oldUri":uri(path),"newUri":"file:///tmp/new.rs"}]});
    assert!(
        rename_edits(&resource, path, Some(7))
            .unwrap_err()
            .contains("file operations")
    );
    assert!(rename_edits(&json!({"changes":{},"documentChanges":[]}), path, Some(7)).is_err());
    for name in ["", " space", "a b", "1name", "name\n"] {
        assert!(!valid_symbol_name(name));
    }
    for name in ["value", "名前", "_next", "$value2"] {
        assert!(valid_symbol_name(name));
    }
}
#[test]
fn reference_results_keep_unopened_file_positions_and_show_truncation() {
    let entry = json!({"uri":"file:///tmp/unopened.rs","range":{"start":{"line":10,"character":5},"end":{"line":10,"character":9}}});
    let (found, capped) = locations(&json!([entry.clone()])).unwrap();
    assert!(!capped);
    assert_eq!(found[0].path, PathBuf::from("/tmp/unopened.rs"));
    assert_eq!(found[0].range.pointer("/start/line"), Some(&json!(10)));
    assert!(
        locations(&Value::Array(vec![entry; MAX_REFERENCES + 1]))
            .unwrap()
            .1
    );
    assert!(locations(&json!([{"uri":"https://example.com/a","range":{}}])).is_err());
}
#[test]
fn rename_input_escape_keeps_document_and_stale_submit_is_rejected() {
    let app = App::new();
    let id = app.active_id();
    app.edit(
        id,
        &EditDelta {
            replaced_range: 0..0,
            inserted: "value".into(),
        },
    );
    let guard = current_guard(&app);
    app.runtime.lock().unwrap().language.panel = Panel::Rename(
        guard.clone(),
        State::new(generate_state_id(), "newValue".into()),
    );
    assert!(app.language_key(KeyCode::Escape, Default::default()));
    assert!(!app.language_rename_open());
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), "value");
    app.runtime.lock().unwrap().language.panel =
        Panel::Rename(guard, State::new(generate_state_id(), "newValue".into()));
    app.bindings(id)
        .unwrap()
        .selection
        .set(TextSelection::collapsed(0));
    app.submit_rename();
    assert!(app.status.get().contains("cancelled"));
    assert!(!app.language_rename_open());
}

#[test]
#[ignore = "requires installed rust-analyzer and rustfmt; run explicitly, serially"]
fn real_rust_analyzer_formats_renames_and_finds_unopened_file_references() {
    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let root = std::env::temp_dir().join(format!("kureha-language-actions-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let fixture = Fixture(std::fs::canonicalize(&root).unwrap());
    std::fs::create_dir(fixture.0.join("src")).unwrap();
    std::fs::write(
        fixture.0.join("Cargo.toml"),
        "[package]\nname = \"kureha_actions_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    let source =
        "mod helper;\nfn main(){let local_value=helper::greet();println!(\"{}\",local_value);}";
    let helper = "pub fn greet() -> i32 { 7 }\n";
    let path = fixture.0.join("src/main.rs");
    let helper_path = fixture.0.join("src/helper.rs");
    std::fs::write(&path, source).unwrap();
    std::fs::write(&helper_path, helper).unwrap();
    let app = App::new();
    let id = {
        let mut rt = app.runtime.lock().unwrap();
        rt.explorer.root = Some(fixture.0.clone());
        rt.workspace.open(path.clone(), source)
    };
    app.activate(id);
    let wait = |label: &str, condition: &dyn Fn() -> bool| {
        let deadline = Instant::now() + Duration::from_secs(50);
        let mut retried = Instant::now();
        while !condition() {
            if label == "references" && retried.elapsed() > Duration::from_secs(2) {
                app.find_references();
                retried = Instant::now();
            }
            app.poll_language();
            app.poll();
            assert!(
                Instant::now() < deadline,
                "{label}: status={} language={}",
                app.status.get(),
                app.language_status.get()
            );
            thread::sleep(Duration::from_millis(30));
        }
    };
    wait("initialize", &|| {
        app.runtime
            .lock()
            .unwrap()
            .language
            .sessions
            .values()
            .any(|session| session.ready)
    });
    app.bindings(id)
        .unwrap()
        .selection
        .set(TextSelection::collapsed(source.find("greet").unwrap() + 2));
    app.find_references();
    wait(
        "references",
        &|| matches!(app.runtime.lock().unwrap().language.panel,Panel::References(ref locations,_) if locations.iter().any(|location|location.path==helper_path)),
    );
    let location = {
        let rt = app.runtime.lock().unwrap();
        let Panel::References(locations, _) = &rt.language.panel else {
            panic!("references")
        };
        locations
            .iter()
            .find(|location| location.path == helper_path)
            .unwrap()
            .clone()
    };
    assert!(
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .find_path(&helper_path)
            .is_none()
    );
    app.language_navigate(location.path, location.range);
    wait("open reference", &|| {
        app.runtime
            .lock()
            .unwrap()
            .workspace
            .find_path(&helper_path)
            .is_some()
    });
    let opened = app.active_id();
    assert_eq!(
        app.bindings(opened).unwrap().selection.get().start().byte,
        helper.find("greet").unwrap()
    );
    eprintln!("APP_REFERENCES_UNOPENED_NAVIGATION_OK");
    app.activate(id);
    app.dismiss_language_panel();
    app.format_document();
    wait("format", &|| {
        app.bindings(id).unwrap().document.get().as_str() != source
    });
    let formatted = app
        .bindings(id)
        .unwrap()
        .document
        .get()
        .as_str()
        .to_string();
    assert!(
        formatted.contains("    let local_value = helper::greet();"),
        "{formatted:?}"
    );
    app.undo(false);
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), source);
    app.undo(true);
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), formatted);
    eprintln!("APP_FORMAT_ONE_UNDO_OK");
    app.bindings(id)
        .unwrap()
        .selection
        .set(TextSelection::collapsed(
            formatted.find("local_value").unwrap() + 2,
        ));
    app.show_rename();
    wait("prepare rename", &|| app.language_rename_open());
    {
        let rt = app.runtime.lock().unwrap();
        let Panel::Rename(_, name) = &rt.language.panel else {
            panic!("rename")
        };
        name.set("renamed_value".into());
    }
    app.submit_rename();
    wait("rename", &|| {
        app.bindings(id)
            .unwrap()
            .document
            .get()
            .as_str()
            .contains("renamed_value")
    });
    assert_eq!(
        app.bindings(id).unwrap().document.get().as_str(),
        formatted.replace("local_value", "renamed_value")
    );
    app.undo(false);
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), formatted);
    eprintln!("APP_RENAME_ONE_UNDO_OK");
    // A real workspace rename must reject its whole result, including the edit
    // to the active buffer, when the declaration belongs to another file.
    app.bindings(id)
        .unwrap()
        .selection
        .set(TextSelection::collapsed(
            formatted.find("greet").unwrap() + 2,
        ));
    app.show_rename();
    wait("prepare cross-file rename", &|| app.language_rename_open());
    {
        let rt = app.runtime.lock().unwrap();
        let Panel::Rename(_, name) = &rt.language.panel else {
            panic!("rename")
        };
        name.set("welcome".into());
    }
    app.submit_rename();
    wait("reject cross-file rename", &|| {
        app.status.get().contains("other files")
    });
    assert_eq!(app.bindings(id).unwrap().document.get().as_str(), formatted);
    assert_eq!(std::fs::read_to_string(&helper_path).unwrap(), helper);
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    assert_eq!(
        app.bindings(opened).unwrap().document.get().as_str(),
        helper
    );
    eprintln!("APP_RENAME_CROSS_FILE_ATOMIC_REJECTION_OK");
}

#[test]
fn unavailable_capabilities_do_not_dispatch_commands_and_escape_cancels_pending_rename() {
    let app = App::new();
    let root = PathBuf::from("/tmp");
    let id = {
        let mut rt = app.runtime.lock().unwrap();
        rt.explorer.root = Some(root.clone());
        rt.workspace
            .open(root.join("language-capabilities.rs"), "fn main() {}")
    };
    app.activate(id);
    let key = session_key("rust", &root);
    app.runtime.lock().unwrap().language.sessions.insert(key.clone(), Session {
        client: None, starting: None, initialize: None, ready: true,
        capabilities: json!({"referencesProvider":false,"documentFormattingProvider":false,"renameProvider":false}),
        pending:BTreeMap::new(), documents:BTreeMap::new(), root,
        language:"rust".into(), label:"fixture".into(), failure:None,
        started:Instant::now(), diagnostic_requests:BTreeMap::new(),
    });
    for method in [
        "textDocument/formatting",
        "textDocument/references",
        "textDocument/prepareRename",
    ] {
        app.language_request(method);
        assert_eq!(app.status.get(), "The server does not support this command");
        assert!(
            app.runtime.lock().unwrap().language.sessions[&key]
                .pending
                .is_empty()
        );
    }
    let guard = current_guard(&app);
    app.runtime
        .lock()
        .unwrap()
        .language
        .sessions
        .get_mut(&key)
        .unwrap()
        .pending
        .insert(
            99,
            Pending {
                guard,
                method: "textDocument/prepareRename".into(),
                attempts: 0,
                started: Instant::now(),
                params: Value::Null,
                hover_generation: None,
                document_version: Some(1),
            },
        );
    assert!(app.dismiss_language_panel());
    assert!(
        app.runtime.lock().unwrap().language.sessions[&key]
            .pending
            .is_empty()
    );
    let snapshots = app.language_snapshots();
    assert!(!app.language_event(&key, LspEvent::Response { id:99, result:json!({"range":{"start":{"line":0,"character":3},"end":{"line":0,"character":7}}}), error:None }, &snapshots));
    assert!(!app.language_rename_open());
}

#[test]
fn unopened_reference_selection_survives_file_worker_navigation_generation() {
    let root = std::env::temp_dir().join(format!("kureha-reference-target-{}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    let root = std::fs::canonicalize(root).unwrap();
    let path = root.join("target.txt");
    let text = "first\nselected symbol\nlast\n";
    std::fs::write(&path, text).unwrap();
    let app = App::new();
    app.language_navigate(
        path.clone(),
        json!({"start":{"line":1,"character":0},"end":{"line":1,"character":8}}),
    );
    let deadline = Instant::now() + Duration::from_secs(3);
    while app
        .runtime
        .lock()
        .unwrap()
        .workspace
        .find_path(&path)
        .is_none()
    {
        app.poll();
        assert!(Instant::now() < deadline, "{}", app.status.get());
        thread::sleep(Duration::from_millis(5));
    }
    let selection = app.bindings(app.active_id()).unwrap().selection.get();
    assert_eq!(
        &text[selection.start().byte..selection.end().byte],
        "selected"
    );
    std::fs::remove_dir_all(root).unwrap();
}
