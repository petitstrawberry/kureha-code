//! direnv: gives the programs Kureha starts for the open folder the
//! environment that direnv loads for it (a Nix flake's dev shell through
//! `use flake`, say): language servers, new terminals and extension programs.
//!
//! direnv's own approval still applies: a blocked `.envrc` loads nothing
//! until "direnv: Allow .envrc" (or `direnv allow` in a terminal).
use kureha_extension::{Cwd, Process, Reply, Request, Value};

/// A Nix dev shell can build for minutes the first time it is entered.
const LOAD_TIMEOUT_MS: u64 = 15 * 60 * 1000;
const ALLOW_TIMEOUT_MS: u64 = 10_000;
/// An `.envrc` direnv has not approved (or that changed since); the error
/// offers `direnv.allow`, which Kureha shows as an "Allow .envrc" button.
pub const BLOCKED: &str = "this folder's .envrc is not allowed yet";

/// Variables from `direnv export json`: a string sets one, `null` removes it.
/// Empty output means nothing changes.
pub fn parse_export(stdout: &str) -> Result<Vec<(String, Option<String>)>, String> {
    if stdout.trim().is_empty() {
        return Ok(Vec::new());
    }
    let value: Value = kureha_extension::serde_json::from_str(stdout)
        .map_err(|e| format!("direnv printed invalid JSON: {e}"))?;
    let object = value
        .as_object()
        .ok_or("direnv printed something other than an object")?;
    object
        .iter()
        .map(|(key, value)| match value {
            Value::Null => Ok((key.clone(), None)),
            Value::String(text) => Ok((key.clone(), Some(text.clone()))),
            _ => Err(format!("direnv gave `{key}` a value that is not a string")),
        })
        .collect()
}

/// `text` without terminal colour codes (`ESC [ … m`), which direnv adds.
fn plain(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' && chars.peek() == Some(&'[') {
            for c in chars.by_ref() {
                if c.is_ascii_alphabetic() {
                    break;
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// The line worth showing from direnv's diagnostics: its error, or the last
/// line it printed.
pub fn reason(stderr: &str) -> String {
    let stderr = plain(stderr);
    let lines: Vec<&str> = stderr
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    lines
        .iter()
        .find(|line| line.contains("error") || line.contains("blocked"))
        .or(lines.last())
        .map(|line| line.trim_start_matches("direnv: ").to_string())
        .unwrap_or_else(|| "direnv failed".into())
}

fn environment() -> Reply {
    let output = match Process::new("direnv")
        .args(["export", "json"])
        .timeout_ms(LOAD_TIMEOUT_MS)
        .run()
    {
        Ok(output) => output,
        Err(error) => return Reply::error(error),
    };
    // A blocked `.envrc` prints its error and nothing on standard output.
    if output.stderr.contains("is blocked") {
        return Reply::error(BLOCKED).offer("direnv.allow");
    }
    if !output.success() {
        return Reply::error(reason(&output.stderr));
    }
    match parse_export(&output.stdout) {
        Ok(vars) => Reply::environment(vars),
        Err(error) => Reply::error(error),
    }
}

fn allow() -> Reply {
    match Process::new("direnv")
        .arg("allow")
        .cwd(Cwd::Workspace)
        .timeout_ms(ALLOW_TIMEOUT_MS)
        .run()
    {
        Ok(output) if output.success() => Reply::status("Allowed .envrc; loading the environment"),
        Ok(output) => Reply::error(reason(&output.stderr)),
        Err(error) => Reply::error(error),
    }
}

fn handle(request: Request) -> Reply {
    match request {
        Request::Activate => Reply::ok(),
        Request::Environment { .. } => environment(),
        Request::Command {
            id: "direnv.allow", ..
        } => allow(),
        _ => Reply::unsupported(),
    }
}
kureha_extension::export!(handle);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exports_set_and_remove_and_errors_are_short() {
        let vars = parse_export(r#"{"PATH":"/nix/store/a/bin:/usr/bin","OLD":null}"#).unwrap();
        assert!(vars.contains(&("PATH".into(), Some("/nix/store/a/bin:/usr/bin".into()))));
        assert!(vars.contains(&("OLD".into(), None)));
        assert!(parse_export("\n").unwrap().is_empty());
        assert!(parse_export("[]").is_err());
        assert_eq!(
            reason(
                "direnv: loading ~/p/.envrc\ndirenv: error /p/.envrc is blocked. Run `direnv allow` to approve its content\n"
            ),
            "error /p/.envrc is blocked. Run `direnv allow` to approve its content"
        );
        assert_eq!(reason(""), "direnv failed");
        // As direnv prints it: in colour.
        assert_eq!(
            reason(
                "\u{1b}[31mdirenv: error /p/.envrc is blocked. Run `direnv allow` to approve its content\u{1b}[0m\n"
            ),
            "error /p/.envrc is blocked. Run `direnv allow` to approve its content"
        );
    }
}
