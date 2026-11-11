use super::*;

/// Escapes text for a WAT string literal.
fn wat_string(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'"' => "\\\"".to_string(),
            b'\\' => "\\\\".to_string(),
            0x20..=0x7e => (b as char).to_string(),
            _ => format!("\\{b:02x}"),
        })
        .collect()
}

/// A guest with a bump allocator. `activate` always answers `{}`; every other
/// call runs `body`, which must leave an `i64` reply on the stack. `data` is
/// placed at offset 0 and `reply(n)` points at the n-th entry.
pub(crate) struct Guest {
    data: Vec<String>,
    body: String,
    imports: String,
    exports_call: bool,
}
impl Guest {
    pub(crate) fn new() -> Self {
        Self {
            data: Vec::new(),
            body: String::new(),
            imports: r#"(import "kureha" "kx_host" (func $host (param i32 i32) (result i64)))
                (import "kureha" "kx_host_take" (func $take (param i32 i32) (result i32)))"#
                .into(),
            exports_call: true,
        }
    }
    /// Adds static bytes and returns WAT for `(i64 (ptr << 32) | len)`.
    pub(crate) fn data(&mut self, text: &str) -> (u32, u32) {
        let offset = self.data.iter().map(|d| d.len() as u32).sum();
        self.data.push(text.into());
        (offset, text.len() as u32)
    }
    pub(crate) fn reply(&mut self, text: &str) -> String {
        let (ptr, len) = self.data(text);
        format!("(i64.const {})", ((ptr as i64) << 32) | len as i64)
    }
    pub(crate) fn body(mut self, body: impl Into<String>) -> Self {
        self.body = body.into();
        self
    }
    pub(crate) fn replying(mut self, text: &str) -> Self {
        self.body = self.reply(text);
        self
    }
    pub(crate) fn imports(mut self, imports: &str) -> Self {
        self.imports = imports.into();
        self
    }
    pub(crate) fn without_call(mut self) -> Self {
        self.exports_call = false;
        self
    }
    pub(crate) fn wasm(mut self) -> Vec<u8> {
        let activate = self.reply("{}");
        let data = self.data.concat();
        let call = if self.exports_call {
            format!(
                r#"(func (export "kx_call") (param $ptr i32) (param $len i32) (result i64)
                    (local $n i32) (local $buf i32)
                    ;; `{{"kind":"a…` is activate.
                    (if (i32.eq (i32.load8_u (i32.add (local.get $ptr) (i32.const 9))) (i32.const 97))
                        (then (return {activate})))
                    {body})"#,
                body = self.body,
            )
        } else {
            String::new()
        };
        let wat = format!(
            r#"(module
                {imports}
                (memory (export "memory") 2)
                (global $heap (mut i32) (i32.const 32768))
                (func $alloc (export "kx_alloc") (param $len i32) (result i32)
                    (local $p i32)
                    (local.set $p (global.get $heap))
                    (global.set $heap (i32.add (global.get $heap) (local.get $len)))
                    (local.get $p))
                (func (export "kx_free") (param i32 i32))
                {call}
                (data (i32.const 0) "{data}"))"#,
            imports = self.imports,
            data = wat_string(&data),
        );
        wat::parse_str(wat).expect("fixture WAT")
    }
}

pub(crate) fn snapshot(text: &str) -> DocumentSnapshot {
    DocumentSnapshot {
        path: "/tmp/example.txt".into(),
        version: 7,
        text: Arc::new(text.into()),
        selection: 0..0,
    }
}

#[test]
fn command_reply_becomes_validated_edits() {
    let wasm = Guest::new()
        .replying(r#"{"status":"Done","edits":[{"start":6,"end":11,"text":"WORLD"},{"start":0,"end":5,"text":"HELLO"}]}"#)
        .wasm();
    let mut runtime = Runtime::new(&wasm, Budgets::default()).unwrap();
    let outcome = runtime.command("x.y", snapshot("hello world")).unwrap();
    assert_eq!(outcome.status.as_deref(), Some("Done"));
    assert_eq!(
        outcome.edits,
        [
            TextEdit {
                range: 0..5,
                text: "HELLO".into()
            },
            TextEdit {
                range: 6..11,
                text: "WORLD".into()
            },
        ]
    );
}

#[test]
fn host_read_round_trips_through_guest_memory() {
    let mut guest = Guest::new();
    let (ptr, len) = guest.data(r#"{"op":"read","start":1,"end":6}"#);
    let wasm = guest
        .body(format!(
            r#"(local.set $n (i32.wrap_i64 (call $host (i32.const {ptr}) (i32.const {len}))))
            (local.set $buf (call $alloc (local.get $n)))
            (drop (call $take (local.get $buf) (local.get $n)))
            (i64.or (i64.shl (i64.extend_i32_u (local.get $buf)) (i64.const 32))
                    (i64.extend_i32_u (local.get $n)))"#
        ))
        .wasm();
    let mut runtime = Runtime::new(&wasm, Budgets::default()).unwrap();
    let reply = runtime
        .invoke(
            &json!({"kind": "command"}),
            Budgets::default().task,
            Some(snapshot("héllo wörld")),
        )
        .unwrap();
    assert_eq!(reply, json!({"text": "éllo"}));
    // Without a document the host refuses, and the guest passes that on.
    let error = runtime.invoke(&json!({"kind": "command"}), Budgets::default().task, None);
    assert!(matches!(error, Err(CallError::Extension(e)) if e.contains("no document")));
}

#[test]
fn infinite_loop_runs_out_of_fuel() {
    let wasm = Guest::new()
        .body("(loop $spin (br $spin)) (i64.const 0)")
        .wasm();
    let mut runtime = Runtime::new(&wasm, Budgets::default()).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        runtime.decorations(snapshot("text"), 0..4),
        Err(CallError::Fuel)
    );
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

#[test]
fn memory_cannot_grow_past_the_limit() {
    let mut guest = Guest::new();
    let denied = guest.reply(r#"{"status":"denied"}"#);
    let grown = guest.reply(r#"{"status":"grown"}"#);
    let pages = MEMORY_LIMIT / 65536; // already 2 pages, so this exceeds it
    let wasm = guest
        .body(format!(
            "(if (result i64) (i32.eq (memory.grow (i32.const {pages})) (i32.const -1))
                (then {denied}) (else {grown}))"
        ))
        .wasm();
    let mut runtime = Runtime::new(&wasm, Budgets::default()).unwrap();
    let outcome = runtime.command("x.y", snapshot("")).unwrap();
    assert_eq!(outcome.status.as_deref(), Some("denied"));
}

#[test]
fn hostile_pointers_are_rejected() {
    let outside = Guest::new()
        .body(format!("(i64.const {})", (0x7fff_0000_i64 << 32) | 16))
        .wasm();
    let mut runtime = Runtime::new(&outside, Budgets::default()).unwrap();
    assert!(matches!(
        runtime.command("x.y", snapshot("")),
        Err(CallError::Trap(e)) if e.contains("outside")
    ));

    let host_outside = Guest::new()
        .body("(call $host (i32.const -16) (i32.const 64))")
        .wasm();
    let mut runtime = Runtime::new(&host_outside, Budgets::default()).unwrap();
    assert!(matches!(
        runtime.command("x.y", snapshot("")),
        Err(CallError::Trap(e)) if e.contains("outside linear memory")
    ));

    let wrong_take = Guest::new()
        .body(
            r#"(drop (call $host (i32.const 0) (i32.const 2)))
            (drop (call $take (i32.const 40000) (i32.const 1)))
            (i64.const 0)"#,
        )
        .wasm();
    let mut runtime = Runtime::new(&wrong_take, Budgets::default()).unwrap();
    assert!(matches!(
        runtime.command("x.y", snapshot("")),
        Err(CallError::Trap(e)) if e.contains("pending reply")
    ));
}

#[test]
fn modules_without_the_abi_are_refused() {
    let wasi = Guest::new()
        .imports(r#"(import "wasi_snapshot_preview1" "fd_write" (func (param i32 i32 i32 i32) (result i32)))"#)
        .replying("{}")
        .wasm();
    assert!(
        Runtime::new(&wasi, Budgets::default())
            .err()
            .unwrap()
            .contains("does not provide")
    );
    let no_call = Guest::new().without_call().wasm();
    assert!(
        Runtime::new(&no_call, Budgets::default())
            .err()
            .unwrap()
            .contains("kx_call")
    );
    assert!(
        Runtime::new(b"not wasm", Budgets::default())
            .err()
            .unwrap()
            .contains("invalid module")
    );
}

#[test]
fn invalid_replies_are_protocol_errors() {
    let text = "日本語";
    for (reply, needle) in [
        (
            r#"{"edits":[{"start":0,"end":6,"text":"x"},{"start":3,"end":9,"text":"y"}]}"#,
            "overlap",
        ),
        (
            r#"{"edits":[{"start":1,"end":3,"text":"x"}]}"#,
            "character boundaries",
        ),
        (
            r#"{"edits":[{"start":0,"end":99,"text":"x"}]}"#,
            "character boundaries",
        ),
        (r#"{"edits":[{"start":0,"end":3}]}"#, "text"),
        (r#"not json"#, "invalid JSON"),
    ] {
        let wasm = Guest::new().replying(reply).wasm();
        let mut runtime = Runtime::new(&wasm, Budgets::default()).unwrap();
        match runtime.command("x.y", snapshot(text)) {
            Err(CallError::Protocol(e)) => assert!(e.contains(needle), "{reply}: {e}"),
            other => panic!("{reply}: {other:?}"),
        }
    }
    let wasm = Guest::new().replying(r#"{"error":"nothing to do"}"#).wasm();
    let mut runtime = Runtime::new(&wasm, Budgets::default()).unwrap();
    assert_eq!(
        runtime.command("x.y", snapshot("")),
        Err(CallError::Extension("nothing to do".into()))
    );
}

#[test]
fn decorations_stay_inside_the_requested_range() {
    let wasm = Guest::new()
        .replying(r#"{"decorations":[{"start":2,"end":4,"style":"warning"}]}"#)
        .wasm();
    let mut runtime = Runtime::new(&wasm, Budgets::default()).unwrap();
    assert_eq!(
        runtime.decorations(snapshot("abcdef"), 0..6).unwrap(),
        [Decoration {
            range: 2..4,
            style: DecorationStyle::Warning
        }]
    );
    assert!(matches!(
        runtime.decorations(snapshot("abcdef"), 0..3),
        Err(CallError::Protocol(e)) if e.contains("outside the requested range")
    ));
}

/// Reports how fast fuel burns here. Run with
/// `cargo test --release fuel_throughput -- --ignored --nocapture`.
#[test]
#[ignore]
fn fuel_throughput() {
    let budgets = Budgets {
        query: 200_000_000,
        ..Budgets::default()
    };
    let wasm = Guest::new()
        .body("(loop $spin (br $spin)) (i64.const 0)")
        .wasm();
    let mut runtime = Runtime::new(&wasm, budgets).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        runtime.decorations(snapshot(""), 0..0),
        Err(CallError::Fuel)
    );
    let seconds = started.elapsed().as_secs_f64();
    let per_ms = budgets.query as f64 / seconds / 1000.0;
    let defaults = Budgets::default();
    println!(
        "{per_ms:.0} fuel/ms: query {:.1} ms, task {:.0} ms, activate {:.0} ms",
        defaults.query as f64 / per_ms,
        defaults.task as f64 / per_ms,
        defaults.activate as f64 / per_ms,
    );
}

/// Runs a real guest built from `examples/extensions/upper-case`:
/// `KUREHA_TEST_EXTENSION_WASM=<path to .wasm> cargo test real_guest -- --ignored`
#[test]
#[ignore]
fn real_guest_upper_cases_the_selection() {
    let path = std::env::var("KUREHA_TEST_EXTENSION_WASM")
        .expect("set KUREHA_TEST_EXTENSION_WASM to the example module");
    let mut runtime = Runtime::new(&std::fs::read(path).unwrap(), Budgets::default()).unwrap();
    let mut document = snapshot("héllo wörld");
    document.selection = 0..6;
    let outcome = runtime
        .command("upper-case.selection", document.clone())
        .unwrap();
    let mut large = snapshot(&"x".repeat(100_000));
    large.selection = 0..100_000;
    let started = std::time::Instant::now();
    for _ in 0..10 {
        runtime
            .command("upper-case.selection", large.clone())
            .unwrap();
    }
    println!(
        "upper-case on a 100 KB selection: {:.2} ms per command",
        started.elapsed().as_secs_f64() * 100.0
    );
    assert_eq!(
        outcome.edits,
        [TextEdit {
            range: 0..6,
            text: "HÉLLO".into()
        }]
    );
    assert_eq!(outcome.status.as_deref(), Some("Upper-cased 5 characters"));
    let collapsed = snapshot("text");
    assert_eq!(
        runtime
            .command("upper-case.selection", collapsed)
            .unwrap()
            .status
            .as_deref(),
        Some("Select some text to upper-case")
    );
}

#[test]
fn looping_on_host_reads_runs_out_of_fuel() {
    let mut guest = Guest::new();
    let (ptr, len) = guest.data(r#"{"op":"read","start":0,"end":400000}"#);
    let wasm = guest
        .body(format!(
            "(loop $again (drop (call $host (i32.const {ptr}) (i32.const {len}))) (br $again))
            (i64.const 0)"
        ))
        .wasm();
    let budgets = Budgets {
        task: 10_000_000,
        ..Budgets::default()
    };
    let mut runtime = Runtime::new(&wasm, budgets).unwrap();
    let started = std::time::Instant::now();
    assert_eq!(
        runtime.command("x.y", snapshot(&"a".repeat(400_000))),
        Err(CallError::Fuel)
    );
    // Without host charges this loop would never stop: each guest iteration
    // costs only a few units while the host copies 400 KB.
    assert!(started.elapsed() < std::time::Duration::from_secs(10));
}

/// Measures Text Tools on its largest accepted input:
/// `KUREHA_TEST_TEXT_TOOLS_WASM=<path> cargo test --release text_tools_budget -- --ignored --nocapture`
#[test]
#[ignore]
fn text_tools_budget() {
    let path = std::env::var("KUREHA_TEST_TEXT_TOOLS_WASM")
        .expect("set KUREHA_TEST_TEXT_TOOLS_WASM to the Text Tools module");
    let budgets = Budgets::default();
    let mut runtime = Runtime::new(&std::fs::read(path).unwrap(), budgets).unwrap();
    let limit = 192 * 1024;
    let mut lines = String::new();
    let mut i = 0u64;
    while lines.len() + 40 < limit {
        i += 1;
        lines.push_str(&format!(
            "line {:08} {}\n",
            (i * 7919) % 100_003,
            "x".repeat(i as usize % 17)
        ));
    }
    let mut json = String::from("[");
    while json.len() + 80 < limit {
        json.push_str(r#"{"name":"item","value":12.50,"tags":["a","b"],"ok":true},"#);
    }
    json.push_str("{}]");
    let words = "parseHttpResponse userId ".repeat(limit / 26);
    let cases = [
        ("text-tools.sort-lines", lines.clone(), false),
        ("text-tools.unique-lines", lines.clone(), false),
        ("text-tools.trim-trailing", lines.clone(), false),
        ("text-tools.json-format-selection", json.clone(), true),
        ("text-tools.json-minify", pretty_json_below(limit), false),
        ("text-tools.count", lines.clone(), false),
        ("text-tools.case-snake", words.clone(), true),
        ("text-tools.base64-encode", lines.clone(), true),
        ("text-tools.url-encode", words.clone(), true),
    ];
    for (command, text, select) in cases {
        let mut document = snapshot(&text);
        if select {
            document.selection = 0..text.len();
        }
        let started = std::time::Instant::now();
        let outcome = runtime.command(command, document);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        let fuel = runtime.last_fuel();
        println!(
            "{command:28} {:>4} KiB  fuel {:>11} ({:>5.1}% of task)  {ms:>6.1} ms  {}",
            text.len() / 1024,
            fuel,
            fuel as f64 * 100.0 / budgets.task as f64,
            match &outcome {
                Ok(o) => o.status.clone().unwrap_or_default(),
                Err(e) => format!("ERROR {e}"),
            }
        );
        assert!(outcome.is_ok(), "{command}: {outcome:?}");
    }
    let mut big = snapshot(&"a\n".repeat(limit));
    big.selection = 0..0;
    assert!(matches!(
        runtime.command("text-tools.sort-lines", big),
        Err(CallError::Extension(e)) if e.contains("192 KiB")
    ));
}
/// Pretty-printed JSON just under `limit` bytes.
fn pretty_json_below(limit: usize) -> String {
    let item = json!({"name": "item", "value": 12, "tags": ["a", "b"], "ok": true});
    let mut items = Vec::new();
    loop {
        items.push(item.clone());
        let text = serde_json::to_string_pretty(&items).unwrap();
        if text.len() + 200 > limit {
            items.pop();
            return serde_json::to_string_pretty(&items).unwrap();
        }
    }
}
