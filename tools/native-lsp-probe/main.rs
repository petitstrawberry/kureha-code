//! Real rust-analyzer acceptance through Kureha's production stdio transport.
#[allow(dead_code)]
#[path = "../../src/lsp.rs"]
mod lsp;

use lsp::{Client, Event, offset_to_position, path_to_uri, uri_to_path};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

type Result<T> = std::result::Result<T, String>;
const SOURCE: &str = "mod numbers;\nfn main() {\n    let answer = numbers::VALUE;\n    let _copy = answer;\n    let text = std::string::String::new();\n    let _text_copy = text;\n}\n";
const NUMBERS: &str = "pub const VALUE: u32 = 42;\npub const OTHER: u64 = 84;\n";

struct Scratch(PathBuf);
impl Scratch {
    fn create() -> Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|e| e.to_string())?
            .as_nanos();
        let path = PathBuf::from(format!(
            "/tmp/kureha-native-lsp-{}-{nonce}",
            std::process::id()
        ));
        fs::create_dir(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let scratch = Self(fs::canonicalize(&path).map_err(|e| e.to_string())?);
        fs::create_dir(scratch.0.join("fixture")).map_err(|e| e.to_string())?;
        // An unconventional target forces Cargo.toml loading; there is no src/main.rs.
        fs::write(scratch.0.join("Cargo.toml"), "[package]\nname = \"kureha_native_lsp_fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n[[bin]]\nname = \"acceptance\"\npath = \"fixture/entry.rs\"\n\n[dependencies]\nnative_values = { path = \"values\" }\n\n[workspace]\nmembers = [\"values\"]\n").map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("fixture/entry.rs"), SOURCE).map_err(|e| e.to_string())?;
        fs::write(
            scratch.0.join("fixture/numbers.rs"),
            "pub use native_values::{VALUE, OTHER};\n",
        )
        .map_err(|e| e.to_string())?;
        fs::create_dir_all(scratch.0.join("values/src")).map_err(|e| e.to_string())?;
        fs::write(
            scratch.0.join("values/Cargo.toml"),
            "[package]\nname = \"native_values\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .map_err(|e| e.to_string())?;
        fs::write(scratch.0.join("values/src/lib.rs"), NUMBERS).map_err(|e| e.to_string())?;
        eprintln!("fixture: {}", scratch.0.display());
        Ok(scratch)
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        if let Err(error) = fs::remove_dir_all(&self.0) {
            eprintln!("fixture cleanup failed for {}: {error}", self.0.display());
        }
    }
}

struct Probe {
    client: Client,
    deadline: Instant,
    server_status: Value,
}
impl Probe {
    fn check_deadline(&self, stage: &str) -> Result<()> {
        if Instant::now() >= self.deadline {
            return Err(format!(
                "deadline exceeded during {stage}; server status: {}",
                self.server_status
            ));
        }
        Ok(())
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        let deadline = self.deadline;
        loop {
            self.check_deadline(method)?;
            let id = self.client.request(method, params.clone())?;
            let response = loop {
                if Instant::now() >= deadline {
                    return Err(format!("request deadline exceeded: {method}"));
                }
                let mut response = None;
                for event in self.client.poll() {
                    match event {
                        Event::Response {
                            id: response_id,
                            result,
                            error,
                        } => {
                            if response_id != id {
                                return Err(format!(
                                    "unexpected response {response_id}, waiting for {id} ({method})"
                                ));
                            }
                            response = Some((result, error));
                        }
                        Event::Notification { method, params } => {
                            if method == "experimental/serverStatus" {
                                eprintln!("server status: {params}");
                                self.server_status = params;
                            } else if matches!(
                                method.as_str(),
                                "window/logMessage" | "window/showMessage"
                            ) || (method == "$/progress"
                                && (params["token"] == "rustAnalyzer/Fetching"
                                    || params["value"]["kind"] != "report"))
                            {
                                eprintln!("{method}: {params}");
                            }
                        }
                        Event::Failed(error) => return Err(error),
                    }
                }
                if let Some(response) = response {
                    break response;
                }
                thread::sleep(Duration::from_millis(10));
            };
            match response {
                (result, None) => return Ok(result),
                (_, Some(error)) if matches!(error.code, -32800 | -32801) => {
                    eprintln!("retry {method}: {}", error.message);
                    if Instant::now() >= deadline {
                        return Err(format!("request retry deadline exceeded: {method}"));
                    }
                    thread::sleep(Duration::from_millis(50));
                }
                (_, Some(error)) => return Err(format!("{method}: {error:?}")),
            }
        }
    }

    fn expect_result(
        &mut self,
        stage: &str,
        method: &str,
        params: Value,
        matches: impl Fn(&Value) -> bool,
    ) -> Result<Value> {
        eprintln!("checking {stage}");
        let mut last_result = Value::Null;
        loop {
            let result = self.request(method, params.clone())?;
            if matches(&result) {
                eprintln!("PASS {stage}: {}", evidence(method, &result));
                return Ok(result);
            }
            if result != last_result {
                eprintln!(
                    "waiting for {stage}; received: {}",
                    evidence(method, &result)
                );
                last_result = result.clone();
            }
            if let Err(error) = self.check_deadline(stage) {
                return Err(format!(
                    "{error}; last result: {}",
                    evidence(method, &result)
                ));
            }
            thread::sleep(Duration::from_millis(100));
        }
    }

    fn run(&mut self, root: &Path) -> Result<()> {
        let root_uri = path_to_uri(root)?;
        let entry = root.join("fixture/entry.rs");
        let uri = path_to_uri(&entry)?;
        let numbers_uri = path_to_uri(&root.join("values/src/lib.rs"))?;
        let initialized = self.request("initialize", json!({
            "processId": std::process::id(),
            "clientInfo": {"name":"kureha-native-lsp-probe","version":env!("CARGO_PKG_VERSION")},
            "rootUri":root_uri,
            "workspaceFolders":[{"uri":root_uri,"name":"kureha_native_lsp_fixture"}],
            "capabilities": {
                "general":{"positionEncodings":["utf-16"]},
                "window":{"workDoneProgress":true},
                "experimental":{"serverStatusNotification":true},
                "textDocument": {
                    "hover":{"contentFormat":["plaintext"]},
                    "definition":{"linkSupport":false},
                    "completion":{"completionItem":{"snippetSupport":false,"insertReplaceSupport":false}}
                }
            },
            "initializationOptions": {
                "cachePriming":{"enable":false},
                "checkOnSave":false,
                "cargo":{"buildScripts":{"enable":false},"extraEnv":{
                    "CARGO_NET_OFFLINE":"true",
                    "CARGO_HOME":root.join("cargo-home")
                }},
                "procMacro":{"enable":false}
            }
        }))?;
        let caps = &initialized["capabilities"];
        ensure(
            caps["hoverProvider"] == true,
            format!("missing hover capability: {caps}"),
        )?;
        ensure(
            caps["definitionProvider"] == true,
            format!("missing definition capability: {caps}"),
        )?;
        ensure(
            caps["completionProvider"].is_object(),
            format!("missing completion capability: {caps}"),
        )?;
        ensure(
            caps["positionEncoding"].is_null() || caps["positionEncoding"] == "utf-16",
            format!("unexpected encoding: {caps}"),
        )?;
        ensure(
            caps["textDocumentSync"]["openClose"] == true
                && matches!(caps["textDocumentSync"]["change"].as_u64(), Some(1 | 2)),
            format!("missing document synchronization: {caps}"),
        )?;
        eprintln!(
            "PASS initialize capabilities; server: {}",
            initialized["serverInfo"]
        );
        self.client.notify("initialized", json!({}))?;

        // Initialization alone is not project loading. Require real Cargo metadata
        // and quiescence before didOpen, then prove it again with an unopened file.
        eprintln!("waiting for Cargo metadata and project loading");
        let mut last_summary = String::new();
        loop {
            let status = self.request("rust-analyzer/analyzerStatus", json!({}))?;
            let text = status
                .as_str()
                .ok_or_else(|| format!("invalid analyzerStatus: {status}"))?;
            let summary = text.split("\nAnalysis:").next().unwrap_or(text);
            if summary != last_summary {
                eprintln!("project status: {summary}");
                last_summary = summary.to_owned();
            }
            if self.server_status["quiescent"] == true
                && self.server_status["health"] == "ok"
                && text.lines().any(|line| {
                    line.starts_with("Loaded ")
                        && line.ends_with(" packages across 1 workspace.")
                        && !line.starts_with("Loaded 0 ")
                })
                && text.lines().any(|line| {
                    line.starts_with("Workspace root folders:")
                        && line.contains(root.join("Cargo.toml").to_str().unwrap())
                })
            {
                eprintln!(
                    "PASS Cargo metadata: {}",
                    text.split("\nAnalysis:").next().unwrap_or(text)
                );
                break;
            }
            self.check_deadline("Cargo metadata/project loading")?;
            thread::sleep(Duration::from_millis(100));
        }

        self.client.notify(
            "textDocument/didOpen",
            json!({"textDocument":{"uri":uri,"languageId":"rust","version":1,"text":SOURCE}}),
        )?;
        self.check_document(SOURCE, &uri, &numbers_uri, "VALUE", "u32", 0)?;

        // A primitive hover alone would also work without rust-src. Navigate to
        // the actual std String declaration to require a usable loaded sysroot.
        let string_at = SOURCE.find("String::new").unwrap();
        let std_definition = self.expect_result(
            "standard library String definition",
            "textDocument/definition",
            position_params(&uri, SOURCE, string_at + 1),
            |result| {
                locations(result).iter().any(|location| {
                    let Some(uri) = location["uri"].as_str() else {
                        return false;
                    };
                    let Ok(path) = uri_to_path(uri) else {
                        return false;
                    };
                    if !path.ends_with("library/alloc/src/string.rs") || path.starts_with(root) {
                        return false;
                    }
                    let Ok(text) = fs::read_to_string(&path) else {
                        return false;
                    };
                    let Some(line) = location["range"]["start"]["line"].as_u64() else {
                        return false;
                    };
                    text.lines()
                        .nth(line as usize)
                        .is_some_and(|line| line.contains("pub struct String"))
                })
            },
        )?;
        ensure(
            !std_definition.is_null(),
            "missing std source definition".into(),
        )?;

        let updated = SOURCE.replace("numbers::VALUE", "numbers::OTHER");
        self.client.notify(
            "textDocument/didChange",
            json!({"textDocument":{"uri":uri,"version":2},"contentChanges":[{"text":updated}]}),
        )?;
        self.check_document(&updated, &uri, &numbers_uri, "OTHER", "u64", 1)?;
        ensure(
            fs::read_to_string(&entry).map_err(|e| e.to_string())? == SOURCE,
            "fixture unexpectedly saved; didChange must prove in-memory synchronization".into(),
        )?;
        self.client
            .notify("textDocument/didClose", json!({"textDocument":{"uri":uri}}))?;
        ensure(
            self.request("shutdown", Value::Null)?.is_null(),
            "shutdown result must be null".into(),
        )?;
        self.client.notify("exit", Value::Null)?;
        loop {
            if let Some(status) = self.client.try_wait()? {
                ensure(status.success(), format!("server exit failed: {status}"))?;
                eprintln!("PASS shutdown/exit: {status}");
                return Ok(());
            }
            self.check_deadline("server exit")?;
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn check_document(
        &mut self,
        source: &str,
        uri: &str,
        numbers_uri: &str,
        symbol: &str,
        ty: &str,
        line: u32,
    ) -> Result<()> {
        let answer_at = source.find("= answer").unwrap() + 2;
        let expected_hover = format!("let answer: {ty}");
        self.expect_result(
            &format!("hover {expected_hover}"),
            "textDocument/hover",
            position_params(uri, source, answer_at + 1),
            |result| {
                result["contents"]["value"]
                    .as_str()
                    .is_some_and(|text| text.lines().any(|line| line.trim() == expected_hover))
            },
        )?;
        let definition_at = source.find(&format!("numbers::{symbol}")).unwrap() + "numbers::".len();
        let expected_range =
            json!({"start":{"line":line,"character":10},"end":{"line":line,"character":15}});
        self.expect_result(
            &format!("unopened Cargo dependency definition {symbol}"),
            "textDocument/definition",
            position_params(uri, source, definition_at + 1),
            |result| {
                locations(result).iter().any(|location| {
                    location["uri"] == numbers_uri && location["range"] == expected_range
                })
            },
        )?;
        let expected_edit = json!({"range":{"start":offset_to_position(source, answer_at).to_value(),"end":offset_to_position(source, answer_at + 6).to_value()},"newText":"answer"});
        self.expect_result(
            "completion label/kind/exact replacement",
            "textDocument/completion",
            position_params(uri, source, answer_at + 3),
            |result| {
                result
                    .as_array()
                    .or_else(|| result["items"].as_array())
                    .is_some_and(|items| {
                        items.iter().any(|item| {
                            item["label"] == "answer"
                                && item["kind"] == 6
                                && item["detail"] == ty
                                && item["textEdit"] == expected_edit
                        })
                    })
            },
        )?;
        Ok(())
    }
}

fn position_params(uri: &str, text: &str, offset: usize) -> Value {
    json!({"textDocument":{"uri":uri},"position":offset_to_position(text, offset).to_value()})
}
fn evidence(method: &str, result: &Value) -> String {
    let selected = if method == "textDocument/completion" {
        result
            .as_array()
            .or_else(|| result["items"].as_array())
            .and_then(|items| items.iter().find(|item| item["label"] == "answer"))
            .unwrap_or(&Value::Null)
    } else {
        result
    };
    selected.to_string().chars().take(2048).collect()
}
fn locations(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(values) => values.iter().collect(),
        Value::Object(_) => vec![value],
        _ => vec![],
    }
}
fn ensure(condition: bool, message: String) -> Result<()> {
    if condition { Ok(()) } else { Err(message) }
}

fn run() -> Result<()> {
    let mut program = "rust-analyzer".to_owned();
    let mut timeout = 180_u64;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--server" => {
                program = args
                    .next()
                    .filter(|s| !s.is_empty())
                    .ok_or("--server requires a program/path")?
            }
            "--timeout-secs" => {
                timeout = args
                    .next()
                    .ok_or("--timeout-secs requires a value")?
                    .parse()
                    .map_err(|_| "invalid timeout")?
            }
            "--help" | "-h" => {
                println!(
                    "kureha-native-lsp-probe [--server PROGRAM_OR_PATH] [--timeout-secs 180]\nRequires real rust-analyzer, Cargo, rustc and matching rust-src. Creates/removes a scratch project under /tmp. No sysroot bypass."
                );
                return Ok(());
            }
            _ => return Err(format!("unknown argument: {arg}")),
        }
    }
    ensure(
        (1..=3600).contains(&timeout),
        "timeout must be 1..=3600 seconds".into(),
    )?;
    let duration = Duration::from_secs(timeout);
    let deadline = Instant::now() + duration;
    let (finished, watchdog) = mpsc::channel::<()>();
    thread::Builder::new().name("lsp-probe-deadline".into()).spawn(move || {
        if matches!(watchdog.recv_timeout(duration + Duration::from_secs(5)), Err(mpsc::RecvTimeoutError::Timeout)) {
            eprintln!("SCARLET_NATIVE_LSP_FAILED: hard deadline ({timeout}s + 5s cleanup grace); scratch path was logged");
            std::process::exit(1);
        }
    }).map_err(|e| e.to_string())?;
    eprintln!(
        "real server: {program}; target_os={}; timeout={timeout}s; inherited PATH={:?}, CARGO={:?}, RUSTC={:?}",
        std::env::consts::OS,
        std::env::var_os("PATH"),
        std::env::var_os("CARGO"),
        std::env::var_os("RUSTC")
    );
    let result = (|| {
        let scratch = Scratch::create()?;
        let mut probe = Probe {
            client: Client::spawn(&program, &[], &scratch.0)?,
            deadline,
            server_status: Value::Null,
        };
        let result = probe.run(&scratch.0);
        if let Err(error) = &result {
            eprintln!("LSP verification failed: {error}");
            eprintln!(
                "server status: {}\nserver stderr (tail):\n{}",
                probe.server_status,
                probe.client.stderr_tail()
            );
        }
        result
    })();
    let _ = finished.send(());
    result?;
    println!("SCARLET_NATIVE_LSP_OK target_os={}", std::env::consts::OS);
    Ok(())
}
fn main() {
    if let Err(error) = run() {
        eprintln!("SCARLET_NATIVE_LSP_FAILED: {error}");
        std::process::exit(1);
    }
}
