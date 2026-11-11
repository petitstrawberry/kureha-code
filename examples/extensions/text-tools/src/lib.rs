//! Text Tools: everyday text transformations as Kureha Code commands.
//!
//! Line, whitespace, JSON and count commands work on the selected lines, or on
//! the whole document when nothing is selected. Case and encoding commands
//! need a selection, so a stray command cannot rewrite a whole file.
use kureha_extension::{Document, Reply, Request};
use std::ops::Range;

pub mod transform;
use transform::Case;

/// Input limit. It keeps every command well inside the host's per-command
/// budget and the 1 MiB reply limit, even when output grows (JSON, Base64).
pub const MAX_INPUT_BYTES: usize = 192 * 1024;
const MAX_OUTPUT_BYTES: usize = 512 * 1024;

fn too_large(bytes: usize) -> Reply {
    Reply::error(format!(
        "Text Tools works on up to {} KiB; this is {} KiB",
        MAX_INPUT_BYTES / 1024,
        bytes.div_ceil(1024)
    ))
}

/// Replaces `range` with `output`, or reports that nothing changed.
fn replace(range: Range<usize>, input: &str, output: String, done: &str) -> Reply {
    if output.len() > MAX_OUTPUT_BYTES {
        return Reply::error("the result is too large to apply");
    }
    if output == input {
        return Reply::status("Text Tools: no changes needed");
    }
    Reply::edit(range, output).with_status(done)
}

/// Commands that read whole lines, or the whole document without a selection.
fn whole_lines(document: &Document, id: &str) -> Reply {
    if document.length > MAX_INPUT_BYTES {
        return too_large(document.length);
    }
    let text = match document.text() {
        Ok(text) => text,
        Err(e) => return Reply::error(e),
    };
    let lines = transform::line_range(&text, &document.selection);
    let input = &text[lines.clone()];
    let (output, done) = match id {
        "sort-lines" => (transform::sort_lines(input), "Sorted lines"),
        "unique-lines" => (transform::unique_lines(input), "Removed duplicate lines"),
        "reverse-lines" => (transform::reverse_lines(input), "Reversed lines"),
        "trim-trailing" => (
            transform::trim_trailing(input),
            "Trimmed trailing whitespace",
        ),
        "json-format" | "json-minify" => {
            let range = if document.selection.is_empty() {
                transform::trimmed(&text, 0..text.len())
            } else {
                transform::trimmed(&text, document.selection.clone())
            };
            let input = &text[range.clone()];
            let result = if id == "json-format" {
                transform::json_format(input)
            } else {
                transform::json_minify(input)
            };
            return match result {
                Ok(output) => replace(range, input, output, "Reformatted JSON"),
                Err(e) => Reply::error(e),
            };
        }
        "count" => {
            let range = if document.selection.is_empty() {
                0..text.len()
            } else {
                document.selection.clone()
            };
            let scope = if document.selection.is_empty() {
                "Document"
            } else {
                "Selection"
            };
            return Reply::status(format!("{scope}: {}", transform::count(&text[range])));
        }
        _ => return Reply::unsupported(),
    };
    replace(lines, input, output, done)
}

/// Commands that rewrite exactly the selection.
fn selection(document: &Document, id: &str) -> Reply {
    let range = document.selection.clone();
    if range.is_empty() {
        return Reply::status("Text Tools: select the text to convert first");
    }
    if range.len() > MAX_INPUT_BYTES {
        return too_large(range.len());
    }
    let input = match document.read(range.clone()) {
        Ok(text) => text,
        Err(e) => return Reply::error(e),
    };
    let case = |case| Ok(transform::convert_case(&input, case));
    let result = match id {
        "case-upper" => case(Case::Upper),
        "case-lower" => case(Case::Lower),
        "case-title" => case(Case::Title),
        "case-snake" => case(Case::Snake),
        "case-constant" => case(Case::Constant),
        "case-camel" => case(Case::Camel),
        "case-pascal" => case(Case::Pascal),
        "case-kebab" => case(Case::Kebab),
        "base64-encode" => Ok(transform::base64_encode(&input)),
        "base64-decode" => transform::base64_decode(&input),
        "url-encode" => Ok(transform::url_encode(&input)),
        "url-decode" => transform::url_decode(&input),
        _ => return Reply::unsupported(),
    };
    match result {
        Ok(output) => replace(range, &input, output, "Converted the selection"),
        Err(e) => Reply::error(e),
    }
}

pub fn handle(request: Request) -> Reply {
    match request {
        Request::Activate => Reply::ok(),
        Request::Command { id, document } => match id.strip_prefix("text-tools.") {
            Some(
                id @ ("sort-lines" | "unique-lines" | "reverse-lines" | "trim-trailing"
                | "json-format" | "json-minify" | "count"),
            ) => whole_lines(&document, id),
            Some(id) => selection(&document, id),
            None => Reply::unsupported(),
        },
        _ => Reply::unsupported(),
    }
}

kureha_extension::export!(handle);
