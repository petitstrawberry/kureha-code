use super::*;
use std::{fs, io::Cursor, time::Instant};

struct Fragmented<R> {
    inner: R,
    chunk: usize,
    interrupt: bool,
}

impl<R: Read> Read for Fragmented<R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.interrupt {
            self.interrupt = false;
            return Err(io::ErrorKind::Interrupted.into());
        }
        self.interrupt = true;
        let n = bytes.len().min(self.chunk);
        self.inner.read(&mut bytes[..n])
    }
}

fn frame(value: &Value) -> Vec<u8> {
    let mut bytes = Vec::new();
    write_frame(&mut bytes, &serde_json::to_vec(value).unwrap()).unwrap();
    bytes
}

#[test]
fn framing_handles_all_small_chunk_sizes_interrupts_unicode_and_coalescing() {
    let first = json!({"jsonrpc":"2.0","id":1,"result":"😀é名前"});
    let second = json!({"jsonrpc":"2.0","method":"test/notice","params":{}});
    let mut stream = frame(&first);
    stream.extend(frame(&second));
    for chunk in 1..=stream.len() {
        let fragmented = Fragmented {
            inner: Cursor::new(&stream),
            chunk,
            interrupt: true,
        };
        let mut reader = BufReader::with_capacity(7, fragmented);
        assert_eq!(
            serde_json::from_slice::<Value>(&read_frame(&mut reader).unwrap().unwrap()).unwrap(),
            first
        );
        assert_eq!(
            serde_json::from_slice::<Value>(&read_frame(&mut reader).unwrap().unwrap()).unwrap(),
            second
        );
        assert_eq!(read_frame(&mut reader).unwrap(), None);
    }
}

#[test]
fn framing_rejects_every_truncation_and_bad_or_oversize_headers() {
    let bytes = frame(&json!({"jsonrpc":"2.0","id":1,"result":"😀"}));
    for end in 1..bytes.len() {
        assert!(
            read_frame(&mut Cursor::new(&bytes[..end])).is_err(),
            "truncation {end}"
        );
    }
    let cases = [
        "\r\n".to_string(),
        "Content-Length: 2\n\n{}".into(),
        "Content-Length: +2\r\n\r\n{}".into(),
        "Content-Length: -1\r\n\r\n".into(),
        "Content-Length: 1x\r\n\r\n".into(),
        "Content-Length: 0\r\n\r\n".into(),
        "Content-Length: 99999999999999999999999999999\r\n\r\n".into(),
        format!("Content-Length: {}\r\n\r\n", MAX_FRAME_BYTES + 1),
        "Content-Length: 2\r\ncontent-length: 2\r\n\r\n{}".into(),
        "Content-Length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=latin1\r\n\r\n{}"
            .into(),
        "Content-Length: 2\r\nÜ: value\r\n\r\n{}".into(),
        "Content Length: 2\r\n\r\n{}".into(),
        "x".repeat(MAX_HEADER_BYTES + 1),
    ];
    for case in cases {
        assert!(
            read_frame(&mut Cursor::new(case.as_bytes())).is_err(),
            "accepted {case:?}"
        );
    }
    let mut supported = Cursor::new(b"content-length:\t2\r\nContent-Type: application/vscode-jsonrpc; charset=UTF8\r\nX-Extra: yes\r\n\r\n{}");
    assert_eq!(read_frame(&mut supported).unwrap(), Some(b"{}".to_vec()));
}

#[test]
fn validates_envelopes_errors_and_preserves_missing_notification_params() {
    let (tx, _rx) = mpsc::sync_channel(8);
    let shared = Shared::default();
    for message in [
        json!([]),
        json!({"jsonrpc":"1.0","id":1,"result":null}),
        json!({"jsonrpc":"2.0","id":1}),
        json!({"jsonrpc":"2.0","id":"1","result":null}),
        json!({"jsonrpc":"2.0","id":1,"result":null,"error":{"code":1,"message":"x"}}),
        json!({"jsonrpc":"2.0","id":1,"error":{"code":"bad","message":"x"}}),
        json!({"jsonrpc":"2.0","id":1,"error":null}),
        json!({"jsonrpc":"2.0","method":5}),
        json!({"jsonrpc":"2.0","method":"x","params":1}),
        json!({"jsonrpc":"2.0","method":"x","id":null}),
        json!({"jsonrpc":"2.0","method":"x","result":null}),
    ] {
        assert!(
            dispatch(message.clone(), 1, &shared, &tx).is_err(),
            "accepted {message}"
        );
    }
    dispatch(json!({"jsonrpc":"2.0","method":"x"}), 1, &shared, &tx).unwrap();
    let inbox = shared.inbox.lock().unwrap();
    assert_eq!(
        inbox.events[0],
        Event::Notification {
            method: "x".into(),
            params: Value::Null
        }
    );
}

#[test]
fn queues_fail_explicitly_at_count_byte_and_frame_limits() {
    let (tx, rx) = mpsc::sync_channel(1);
    enqueue(&tx, &Value::Null).unwrap();
    assert!(enqueue(&tx, &Value::Null).unwrap_err().contains("full"));
    drop(rx);
    assert!(
        enqueue(&tx, &Value::Null)
            .unwrap_err()
            .contains("disconnected")
    );
    assert!(
        enqueue(&tx, &Value::String("x".repeat(MAX_FRAME_BYTES)))
            .unwrap_err()
            .contains("exceeds")
    );
    let shared = Shared::default();
    let notice = || Event::Notification {
        method: "x".into(),
        params: Value::Null,
    };
    shared.event(notice(), MAX_QUEUED_BYTES).unwrap();
    assert!(shared.event(notice(), 1).is_err());
    let shared = Shared::default();
    for _ in 0..MAX_EVENTS {
        shared.event(notice(), 1).unwrap();
    }
    assert!(shared.event(notice(), 1).is_err());
}

#[test]
fn configuration_defaults_and_terminal_failure_order_are_explicit() {
    assert_eq!(
        server_reply(
            json!("empty"),
            "workspace/configuration",
            &json!({"items":[]})
        ),
        json!({"jsonrpc":"2.0","id":"empty","result":[]})
    );
    assert_eq!(
        server_reply(
            json!(0),
            "workspace/configuration",
            &json!({"items":[{},{}]})
        )["result"],
        json!([null, null])
    );
    let shared = Shared::default();
    shared.fail("first failure");
    assert!(
        shared
            .event(
                Event::Notification {
                    method: "late".into(),
                    params: Value::Null
                },
                1
            )
            .is_err()
    );
    shared.fail("second failure");
    let inbox = shared.inbox.lock().unwrap();
    assert!(inbox.events.is_empty());
    assert_eq!(inbox.failure.as_deref(), Some("first failure"));
}

#[test]
fn utf16_positions_cover_surrogates_combining_characters_and_all_line_endings() {
    let text = "a😀e\u{301}名前\r\nβ\rlast\n";
    for offset in 0..=text.len() {
        if text.is_char_boundary(offset) && text.as_bytes().get(offset) != Some(&b'\n') {
            let position = offset_to_position(text, offset);
            assert_eq!(
                position_to_offset(text, position),
                Some(offset),
                "offset {offset}"
            );
        }
    }
    assert_eq!(
        offset_to_position(text, 2),
        Position {
            line: 0,
            character: 1
        }
    );
    assert_eq!(
        offset_to_position(text, 5),
        Position {
            line: 0,
            character: 3
        }
    );
    assert_eq!(
        position_to_offset(
            text,
            Position {
                line: 0,
                character: 2
            }
        ),
        None
    );
    assert_eq!(
        position_to_offset(
            text,
            Position {
                line: 0,
                character: 999
            }
        ),
        text.find('\r')
    );
    assert_eq!(
        position_to_offset(
            text,
            Position {
                line: 4,
                character: 0
            }
        ),
        None
    );
    assert_eq!(position_to_offset("", Position::default()), Some(0));
    assert_eq!(
        position_to_offset(
            "x\n",
            Position {
                line: 1,
                character: 0
            }
        ),
        Some(2)
    );
    let cr = text.find('\r').unwrap();
    assert_eq!(
        offset_to_position(text, cr),
        offset_to_position(text, cr + 1)
    );
    assert_eq!(
        offset_to_position(text, usize::MAX),
        Position {
            line: 3,
            character: 0
        }
    );
    for value in [
        json!({"line":-1,"character":0}),
        json!({"line":0,"character":1.5}),
        json!({"line":2147483648u64,"character":0}),
    ] {
        assert!(Position::from_value(&value).is_err());
    }
    let position = Position {
        line: 1,
        character: 2,
    };
    assert_eq!(
        Position::from_value(&position.to_value()).unwrap(),
        position
    );
}

#[test]
fn file_uri_round_trips_reserved_unicode_and_literal_percent_once() {
    #[cfg(unix)]
    let path = Path::new("/tmp/名前 a+b#c?d%20.rs");
    #[cfg(not(unix))]
    let path = Path::new("C:\\tmp\\名前 a+b#c?d%20.rs");
    let uri = path_to_uri(path).unwrap();
    assert!(uri.contains("%20a%2Bb%23c%3Fd%2520.rs"));
    assert_eq!(uri_to_path(&uri).unwrap(), path);
    assert!(path_to_uri(Path::new("relative.rs")).is_err());
    #[cfg(unix)]
    {
        assert_eq!(
            uri_to_path("FILE://LOCALHOST/tmp/a+b").unwrap(),
            Path::new("/tmp/a+b")
        );
        assert_eq!(uri_to_path("file:/tmp/x").unwrap(), Path::new("/tmp/x"));
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(b"/tmp/\xff\\x".to_vec()));
        assert_eq!(uri_to_path(&path_to_uri(&path).unwrap()).unwrap(), path);
    }
    for uri in [
        "https://example/a",
        "file://remote/tmp/a",
        "file:relative",
        "file:///a%",
        "file:///a%GG",
        "file:///a%00",
        "file:///a%2fb",
        "file:///a?x",
        "file:///a#x",
        "file:///a b",
        "file:///a\nx",
    ] {
        assert!(uri_to_path(uri).is_err(), "accepted {uri}");
    }
}

fn edit(start: u32, end: u32, new_text: &str) -> Value {
    json!({"range":{"start":{"line":0,"character":start},"end":{"line":0,"character":end}},"newText":new_text})
}

#[test]
fn edits_validate_before_application_and_preserve_equal_position_insert_order() {
    let text = "a😀bc";
    let edits = json!([edit(4, 5, "!"), edit(1, 3, "名前")]);
    let converted = text_edits(text, &edits).unwrap();
    assert_eq!(converted[0].range, 6..7);
    assert_eq!(converted[1].range, 1..5);
    assert_eq!(apply_text_edits(text, &edits).unwrap(), "a名前b!");
    assert_eq!(
        apply_text_edits(
            "abc",
            &json!([edit(1, 1, "X"), edit(1, 1, "Y"), edit(1, 2, "Z")])
        )
        .unwrap(),
        "aXYZc"
    );
    assert_eq!(
        apply_text_edits("abc", &json!([edit(99, 100, "!")])).unwrap(),
        "abc!"
    );
    for edits in [
        json!([edit(1, 4, ""), edit(3, 5, "")]),
        json!([edit(2, 3, "")]),
        json!([edit(4, 1, "")]),
        json!([{"range":{},"newText":""}]),
        json!({}),
        json!([edit(1, 3, ""), edit(1, 1, "x")]),
    ] {
        assert!(text_edits(text, &edits).is_err(), "accepted {edits}");
    }
    assert_eq!(apply_text_edits(text, &json!([])).unwrap(), text);
}

fn fixture(mode: &str) -> Client {
    const HELPER: &str = include_str!("../tests/helpers/lsp_stdio_server.py");
    let python = ["/usr/bin/python3", "/opt/homebrew/bin/python3", "python3"]
        .into_iter()
        .find(|p| {
            Command::new(p)
                .arg("--version")
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .is_ok_and(|s| s.success())
        })
        .expect("Python 3 is needed for the subprocess transport fixture");
    Client::spawn(
        python,
        &["-u".into(), "-c".into(), HELPER.into(), mode.into()],
        &std::env::temp_dir(),
    )
    .unwrap()
}

fn wait_event(client: &Client, matches: impl Fn(&Event) -> bool) -> Event {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        for event in client.poll() {
            if matches(&event) {
                return event;
            }
            if let Event::Failed(error) = event {
                panic!("Unexpected transport failure: {error}");
            }
        }
        assert!(
            Instant::now() < deadline,
            "Timed out; stderr: {}",
            client.stderr_tail()
        );
        thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn process_roundtrip_handles_async_responses_errors_server_requests_and_stderr() {
    let client = fixture("normal");
    let expected = json!({"text":"😀名前", "nested":[1,null]});
    let id = client.request("fixture/echo", expected.clone()).unwrap();
    let response = wait_event(
        &client,
        |e| matches!(e, Event::Response { id:i, .. } if *i == id),
    );
    assert_eq!(
        response,
        Event::Response {
            id,
            result: expected,
            error: None
        }
    );
    assert!(client.stderr_tail().ends_with("fixture stderr tail\n"));
    assert!(client.stderr_tail().len() <= MAX_STDERR_BYTES);
    let id = client.request("fixture/error", Value::Null).unwrap();
    let response = wait_event(
        &client,
        |e| matches!(e, Event::Response { id:i, .. } if *i == id),
    );
    assert_eq!(
        response,
        Event::Response {
            id,
            result: Value::Null,
            error: Some(ResponseError {
                code: -32800,
                message: "cancelled".into(),
                data: Some(json!({"detail":7}))
            })
        }
    );
    client.notify("fixture/serverRequests", json!({})).unwrap();
    let Event::Notification { params, .. } = wait_event(
        &client,
        |e| matches!(e, Event::Notification { method, .. } if method == "fixture/replies"),
    ) else {
        unreachable!()
    };
    let replies = params["replies"].as_array().unwrap();
    assert_eq!(replies[0]["id"], "configuration-string-id");
    assert_eq!(replies[0]["result"], json!([null, null]));
    assert_eq!(replies[1]["id"], -7);
    assert_eq!(replies[1]["error"]["code"], -32601);
    assert_eq!(replies[2]["result"]["applied"], false);
    for reply in &replies[3..6] {
        assert_eq!(reply["result"], Value::Null);
        assert!(reply.get("error").is_none());
    }
    assert_eq!(replies[6]["error"]["code"], -32602);
    assert_eq!(replies[7]["error"]["code"], -32601);
    client.notify("fixture/exit", Value::Null).unwrap();
    wait_event(&client, |e| matches!(e, Event::Failed(_)));
    assert!(client.poll().is_empty());
    assert!(client.notify("after/exit", Value::Null).is_err());
}

#[test]
fn process_reports_bad_json_truncation_eof_and_useful_stderr_once() {
    for (mode, expected) in [
        ("malformed", "Invalid LSP JSON"),
        ("truncated", "LSP body"),
        ("stderr_exit", "stdout closed"),
    ] {
        let client = fixture(mode);
        let Event::Failed(error) = wait_event(&client, |e| matches!(e, Event::Failed(_))) else {
            unreachable!()
        };
        assert!(error.contains(expected), "{error}");
        if mode == "stderr_exit" {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !client
                .stderr_tail()
                .contains("fixture fatal diagnostic: 名前")
                && Instant::now() < deadline
            {
                thread::sleep(Duration::from_millis(5));
            }
            assert!(
                client
                    .stderr_tail()
                    .contains("fixture fatal diagnostic: 名前")
            );
        }
        assert!(client.poll().is_empty());
    }
    assert!(
        Client::spawn(
            "/nonexistent/kureha-language-server",
            &[],
            &std::env::temp_dir()
        )
        .is_err()
    );
}

#[test]
fn blocked_stdin_does_not_block_calls_and_drop_kills_and_reaps() {
    let client = fixture("no_read");
    wait_event(
        &client,
        |e| matches!(e, Event::Notification { method, .. } if method == "fixture/ready"),
    );
    let pid = client.child.lock().unwrap().id();
    assert!(client.request("bad", json!(7)).is_err());
    assert!(client.notify("", Value::Null).is_err());
    let start = Instant::now();
    let large = json!({"text":"x".repeat(128 * 1024)});
    let mut full = false;
    for _ in 0..MAX_OUTGOING + 4 {
        if let Err(error) = client.request("fixture/blocked", large.clone()) {
            assert!(error.contains("full"), "{error}");
            full = true;
            break;
        }
        assert!(client.try_poll().is_empty());
    }
    assert!(full);
    assert!(start.elapsed() < Duration::from_secs(3));
    let start = Instant::now();
    drop(client);
    assert!(start.elapsed() < Duration::from_secs(3));
    #[cfg(unix)]
    assert!(
        !Command::new("/bin/kill")
            .args(["-0", &pid.to_string()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .unwrap()
            .success(),
        "child {pid} was not reaped"
    );
}

#[test]
fn incoming_flood_produces_one_failure_instead_of_unbounded_growth() {
    let client = fixture("flood");
    let deadline = Instant::now() + Duration::from_secs(10);
    while !client.shared.stopped.load(Ordering::Acquire) {
        assert!(Instant::now() < deadline);
        thread::sleep(Duration::from_millis(5));
    }
    let events = client.poll();
    assert_eq!(events.len(), MAX_EVENTS + 1);
    assert!(matches!(events.last(), Some(Event::Failed(error)) if error.contains("queue is full")));
    assert!(client.poll().is_empty());
}

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "kureha-lsp-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Run explicitly with KUREHA_RUST_ANALYZER=/absolute/path/to/rust-analyzer.
/// Uses a tiny standalone crate; never builds the editor or a system image.
#[test]
#[ignore = "requires a real rust-analyzer; set KUREHA_RUST_ANALYZER and run --ignored"]
fn real_rust_analyzer_initialize_hover_completion_format_and_shutdown() {
    let program = std::env::var("KUREHA_RUST_ANALYZER").expect("Set KUREHA_RUST_ANALYZER");
    let root = Scratch::new();
    fs::create_dir(root.0.join("src")).unwrap();
    fs::write(
        root.0.join("Cargo.toml"),
        "[package]\nname = \"lsp_transport_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
    )
    .unwrap();
    // Put non-BMP text before the queried token to exercise actual UTF-16
    // negotiation against the server, rather than merely ASCII positions.
    let source = "fn main(){let _label = \"😀名前\";let answer: u32 = 42;let next = answer + 1;}\n";
    let path = root.0.join("src/main.rs");
    fs::write(&path, source).unwrap();
    let uri = path_to_uri(&path).unwrap();
    let root_uri = path_to_uri(&root.0).unwrap();
    let client = Client::spawn(&program, &[], &root.0).unwrap();
    let initialize = client.request("initialize", json!({
        "processId":std::process::id(), "rootUri":root_uri,
        "capabilities":{"general":{"positionEncodings":["utf-16"]},"workspace":{"configuration":true},"textDocument":{"hover":{"contentFormat":["plaintext"]}}},
        "initializationOptions":{"checkOnSave":false,"cargo":{"buildScripts":{"enable":false}},"procMacro":{"enable":false}}
    })).unwrap();
    let Event::Response { result, error, .. } = wait_event(
        &client,
        |e| matches!(e, Event::Response {id,..} if *id == initialize),
    ) else {
        unreachable!()
    };
    assert_eq!(error, None);
    assert!(result["capabilities"]["hoverProvider"].as_bool().unwrap());
    client.notify("initialized", json!({})).unwrap();
    client
        .notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"rust","version":1,"text":source}}),
        )
        .unwrap();
    // Project loading is asynchronous; retry transient content-modified errors
    // and null results while the real server discovers the tiny crate.
    let hover_position = offset_to_position(source, source.rfind("answer").unwrap() + 1).to_value();
    let deadline = Instant::now() + Duration::from_secs(60);
    let hover = loop {
        let id = client
            .request(
                "textDocument/hover",
                json!({"textDocument":{"uri":uri},"position":hover_position}),
            )
            .unwrap();
        let Event::Response { result, error, .. } = wait_event(
            &client,
            |e| matches!(e, Event::Response {id:i,..} if *i == id),
        ) else {
            unreachable!()
        };
        if error.is_none() && !result.is_null() {
            break result;
        }
        assert!(
            Instant::now() < deadline,
            "No hover; {error:?}; stderr: {}",
            client.stderr_tail()
        );
        thread::sleep(Duration::from_millis(100));
    };
    assert!(hover.to_string().contains("u32"), "{hover}");
    let completion_offset = source.rfind("answer").unwrap() + 3;
    let id = client.request("textDocument/completion", json!({"textDocument":{"uri":uri},"position":offset_to_position(source, completion_offset).to_value()})).unwrap();
    let Event::Response {
        result: completion,
        error,
        ..
    } = wait_event(
        &client,
        |e| matches!(e, Event::Response {id:i,..} if *i == id),
    )
    else {
        unreachable!()
    };
    assert_eq!(error, None);
    assert!(completion.to_string().contains("answer"), "{completion}");
    let id = client
        .request(
            "textDocument/formatting",
            json!({"textDocument":{"uri":uri},"options":{"tabSize":4,"insertSpaces":true}}),
        )
        .unwrap();
    let Event::Response {
        result: formatting,
        error,
        ..
    } = wait_event(
        &client,
        |e| matches!(e, Event::Response {id:i,..} if *i == id),
    )
    else {
        unreachable!()
    };
    assert_eq!(error, None);
    let formatted = apply_text_edits(source, &formatting).unwrap();
    assert!(formatted.contains("fn main() {\n"), "{formatted}");
    client
        .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))
        .unwrap();
    let shutdown = client.request("shutdown", Value::Null).unwrap();
    let Event::Response { error, .. } = wait_event(
        &client,
        |e| matches!(e, Event::Response {id,..} if *id == shutdown),
    ) else {
        unreachable!()
    };
    assert_eq!(error, None);
    client.notify("exit", Value::Null).unwrap();
    wait_event(&client, |e| matches!(e, Event::Failed(_)));
    assert!(
        client
            .child
            .lock()
            .unwrap()
            .try_wait()
            .unwrap()
            .unwrap()
            .success()
    );
    eprintln!(
        "Real rust-analyzer: initialize, UTF-16 hover, completion, rustfmt edits, shutdown/exit passed"
    );
}
