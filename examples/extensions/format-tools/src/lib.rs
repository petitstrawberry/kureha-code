//! Format Tools: Code: Format Document (and format-on-save) through the
//! formatter you already use on the command line. The tool reads the document
//! from standard input, finds the project's configuration from the document's
//! folder, and prints the result, which replaces the document.
use kureha_extension::{Cwd, Document, Process, Reply, Request};

/// Formatters can take a moment to start (Prettier in particular).
const TIMEOUT_MS: u64 = 10_000;

/// The program and arguments that format `language`. `path` names the file
/// for configuration lookup; unsaved documents use a placeholder name.
pub fn plan(
    language: &str,
    path: &str,
    tab_size: u32,
    insert_spaces: bool,
) -> Option<(&'static str, Vec<String>)> {
    let name = |placeholder: &str| {
        if path.is_empty() {
            placeholder.to_owned()
        } else {
            path.to_owned()
        }
    };
    match language {
        "python" => Some((
            "ruff",
            vec![
                "format".into(),
                "--stdin-filename".into(),
                name("stdin.py"),
                "-".into(),
            ],
        )),
        "javascript" | "javascriptreact" | "typescript" | "typescriptreact" => {
            let extension = match language {
                "javascript" => "js",
                "javascriptreact" => "jsx",
                "typescript" => "ts",
                _ => "tsx",
            };
            // The editor's indentation applies only without a Prettier config.
            let mut args = vec![
                "--stdin-filepath".into(),
                name(&format!("stdin.{extension}")),
                "--config-precedence".into(),
                "prefer-file".into(),
                "--tab-width".into(),
                tab_size.to_string(),
            ];
            if !insert_spaces {
                args.push("--use-tabs".into());
            }
            Some(("prettier", args))
        }
        "c" | "cpp" => Some((
            "clang-format",
            vec![format!(
                "--assume-filename={}",
                name(if language == "c" {
                    "stdin.c"
                } else {
                    "stdin.cpp"
                })
            )],
        )),
        _ => None,
    }
}

fn format(document: &Document, language: &str, tab_size: u32, insert_spaces: bool) -> Reply {
    let Some((program, args)) = plan(language, &document.path, tab_size, insert_spaces) else {
        return Reply::error(format!("Format Tools does not format {language}"));
    };
    let output = match Process::new(program)
        .args(args)
        .stdin_document()
        .cwd(Cwd::Document)
        .timeout_ms(TIMEOUT_MS)
        .run()
    {
        Ok(output) => output,
        Err(e) => return Reply::error(e),
    };
    if !output.success() {
        return Reply::error(format!("{program}: {}", output.failure()));
    }
    // Skip the edit when nothing changed, so the document stays unmodified.
    if document.text().is_ok_and(|text| text == output.stdout) {
        return Reply::unchanged();
    }
    Reply::edit(0..document.length, output.stdout)
}

fn handle(request: Request) -> Reply {
    match request {
        Request::Activate => Reply::ok(),
        Request::Format {
            document,
            language,
            tab_size,
            insert_spaces,
        } => format(&document, language, tab_size, insert_spaces),
        _ => Reply::unsupported(),
    }
}

kureha_extension::export!(handle);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn each_language_maps_to_its_tool_with_the_file_name() {
        let (program, args) = plan("python", "/p/app.py", 4, true).unwrap();
        assert_eq!(program, "ruff");
        assert_eq!(args, ["format", "--stdin-filename", "/p/app.py", "-"]);
        let (program, args) = plan("typescriptreact", "", 2, false).unwrap();
        assert_eq!(program, "prettier");
        assert_eq!(&args[..2], ["--stdin-filepath", "stdin.tsx"]);
        assert!(args.ends_with(&["2".into(), "--use-tabs".into()]));
        let (program, args) = plan("cpp", "/p/main.cc", 4, true).unwrap();
        assert_eq!(program, "clang-format");
        assert_eq!(args, ["--assume-filename=/p/main.cc"]);
        assert!(plan("rust", "/p/main.rs", 4, true).is_none());
    }

    #[test]
    fn every_planned_program_is_declared_in_the_manifest() {
        let manifest = include_str!("../extension.toml");
        let declared = manifest
            .lines()
            .find_map(|line| line.strip_prefix("process = "))
            .unwrap();
        for language in [
            "python",
            "javascript",
            "javascriptreact",
            "typescript",
            "typescriptreact",
            "c",
            "cpp",
        ] {
            let (program, _) = plan(language, "", 4, true).unwrap();
            assert!(declared.contains(&format!("\"{program}\"")), "{program}");
            assert!(manifest.contains(&format!("\"{language}\"")), "{language}");
        }
    }
}
