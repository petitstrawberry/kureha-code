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
