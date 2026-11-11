//! Pure text transformations. Nothing here talks to the host.
use std::ops::Range;

/// The line ending used for rejoining: CRLF when the text uses it.
fn line_ending(text: &str) -> &'static str {
    if text.contains("\r\n") { "\r\n" } else { "\n" }
}

/// The byte range of whole lines a line command should rewrite.
///
/// An empty selection means the whole document. A partial selection grows to
/// whole lines, but a selection ending at the start of a line does not take
/// that line. The final line ending is never part of the range, so the
/// document keeps (or keeps lacking) its trailing newline.
pub fn line_range(text: &str, selection: &Range<usize>) -> Range<usize> {
    let content_end = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .map_or(text.len(), str::len);
    if selection.is_empty() {
        return 0..content_end;
    }
    let start = text[..selection.start].rfind('\n').map_or(0, |i| i + 1);
    let mut end = selection.end;
    if end > start && text[..end].ends_with('\n') {
        end -= 1;
        if text[..end].ends_with('\r') {
            end -= 1;
        }
    } else {
        end = text[end..].find('\n').map_or(text.len(), |i| end + i);
        if text[..end].ends_with('\r') {
            end -= 1;
        }
    }
    start..end.max(start)
}

fn map_lines(text: &str, f: impl FnOnce(Vec<&str>) -> Vec<&str>) -> String {
    let eol = line_ending(text);
    f(text.split(eol).collect()).join(eol)
}
pub fn sort_lines(text: &str) -> String {
    map_lines(text, |mut lines| {
        lines.sort_unstable();
        lines
    })
}
pub fn unique_lines(text: &str) -> String {
    map_lines(text, |lines| {
        let mut seen = std::collections::HashSet::new();
        lines
            .into_iter()
            .filter(|line| seen.insert(*line))
            .collect()
    })
}
pub fn reverse_lines(text: &str) -> String {
    map_lines(text, |mut lines| {
        lines.reverse();
        lines
    })
}
pub fn trim_trailing(text: &str) -> String {
    map_lines(text, |lines| {
        lines
            .into_iter()
            .map(|line| line.trim_end_matches([' ', '\t']))
            .collect()
    })
}

/// The range without surrounding whitespace, so reformatting keeps it.
pub fn trimmed(text: &str, range: Range<usize>) -> Range<usize> {
    let slice = &text[range.clone()];
    let start = range.start + (slice.len() - slice.trim_start().len());
    let end = range.start + slice.trim_end().len();
    start..end.max(start)
}

pub fn json_format(text: &str) -> Result<String, String> {
    json_format_indented(text, "  ")
}
/// Pretty-prints JSON with `indent` per level (spaces or a tab).
pub fn json_format_indented(text: &str, indent: &str) -> Result<String, String> {
    use serde::Serialize;
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    let mut out = Vec::new();
    let formatter = serde_json::ser::PrettyFormatter::with_indent(indent.as_bytes());
    value
        .serialize(&mut serde_json::Serializer::with_formatter(
            &mut out, formatter,
        ))
        .map_err(|e| e.to_string())?;
    String::from_utf8(out).map_err(|e| e.to_string())
}
pub fn json_minify(text: &str) -> Result<String, String> {
    let value: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("not valid JSON: {e}"))?;
    serde_json::to_string(&value).map_err(|e| e.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Case {
    Upper,
    Lower,
    Title,
    Snake,
    Constant,
    Camel,
    Pascal,
    Kebab,
}

/// Splits an identifier-like chunk into words at separators and case changes:
/// `parseHTTPResponse2xx` → `parse`, `HTTP`, `Response2xx`.
fn words(chunk: &str) -> Vec<String> {
    let chars: Vec<char> = chunk.chars().collect();
    let mut words = Vec::new();
    let mut word = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !word.is_empty() {
                words.push(std::mem::take(&mut word));
            }
            continue;
        }
        let previous = i.checked_sub(1).map(|j| chars[j]);
        let next = chars.get(i + 1).copied();
        let boundary = c.is_uppercase()
            && previous.is_some_and(|p| {
                p.is_lowercase()
                    || p.is_ascii_digit()
                    || (p.is_uppercase() && next.is_some_and(char::is_lowercase))
            });
        if boundary && !word.is_empty() {
            words.push(std::mem::take(&mut word));
        }
        word.push(c);
    }
    if !word.is_empty() {
        words.push(word);
    }
    words
}
fn capitalized(word: &str) -> String {
    let mut chars = word.chars();
    chars
        .next()
        .map(|first| {
            first
                .to_uppercase()
                .chain(chars.flat_map(char::to_lowercase))
                .collect()
        })
        .unwrap_or_default()
}

/// Converts each whitespace-separated chunk, keeping the whitespace itself.
pub fn convert_case(text: &str, case: Case) -> String {
    match case {
        Case::Upper => return text.to_uppercase(),
        Case::Lower => return text.to_lowercase(),
        _ => {}
    }
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while !rest.is_empty() {
        let space = rest.len() - rest.trim_start().len();
        out.push_str(&rest[..space]);
        rest = &rest[space..];
        let end = rest.find(char::is_whitespace).unwrap_or(rest.len());
        let chunk = &rest[..end];
        rest = &rest[end..];
        if case == Case::Title {
            // Title Case keeps punctuation; it only recapitalises the chunk.
            out.push_str(&capitalized(chunk));
            continue;
        }
        let words = words(chunk);
        let lower = || words.iter().map(|w| w.to_lowercase());
        let converted = match case {
            Case::Snake => lower().collect::<Vec<_>>().join("_"),
            Case::Kebab => lower().collect::<Vec<_>>().join("-"),
            Case::Constant => words
                .iter()
                .map(|w| w.to_uppercase())
                .collect::<Vec<_>>()
                .join("_"),
            Case::Pascal => words.iter().map(|w| capitalized(w)).collect(),
            Case::Camel => words
                .iter()
                .enumerate()
                .map(|(i, w)| {
                    if i == 0 {
                        w.to_lowercase()
                    } else {
                        capitalized(w)
                    }
                })
                .collect(),
            Case::Upper | Case::Lower | Case::Title => unreachable!(),
        };
        out.push_str(&converted);
    }
    out
}

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len().div_ceil(3) * 4);
    for chunk in text.as_bytes().chunks(3) {
        let bytes = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]);
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(BASE64[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}
pub fn base64_decode(text: &str) -> Result<String, String> {
    let symbols: Vec<u8> = text.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    let data = symbols
        .strip_suffix(b"==")
        .or_else(|| symbols.strip_suffix(b"="))
        .unwrap_or(&symbols);
    if !symbols.len().is_multiple_of(4) || data.len() % 4 == 1 {
        return Err("not valid Base64".into());
    }
    let mut bits = 0u32;
    let mut count = 0;
    let mut bytes = Vec::with_capacity(data.len() * 3 / 4);
    for &symbol in data {
        let value = match symbol {
            b'A'..=b'Z' => symbol - b'A',
            b'a'..=b'z' => symbol - b'a' + 26,
            b'0'..=b'9' => symbol - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return Err("not valid Base64".into()),
        };
        bits = bits << 6 | u32::from(value);
        count += 6;
        if count >= 8 {
            count -= 8;
            bytes.push((bits >> count) as u8);
        }
    }
    String::from_utf8(bytes).map_err(|_| "the decoded bytes are not UTF-8 text".into())
}

pub fn url_encode(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("%{byte:02X}"));
        }
    }
    out
}
pub fn url_decode(text: &str) -> Result<String, String> {
    let input = text.as_bytes();
    let mut bytes = Vec::with_capacity(input.len());
    let mut i = 0;
    while i < input.len() {
        if input[i] == b'%' {
            let hex = input
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok())
                .ok_or("a % is not followed by two hex digits")?;
            bytes.push(hex);
            i += 3;
        } else {
            bytes.push(input[i]);
            i += 1;
        }
    }
    String::from_utf8(bytes).map_err(|_| "the decoded bytes are not UTF-8 text".into())
}

pub fn count(text: &str) -> String {
    let lines = if text.is_empty() {
        0
    } else {
        text.lines().count()
    };
    format!(
        "{lines} lines, {} words, {} characters",
        text.split_whitespace().count(),
        text.chars().count()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn line_ranges_grow_to_whole_lines_and_keep_the_final_newline() {
        let text = "b\na\nc\n";
        assert_eq!(line_range(text, &(0..0)), 0..5);
        assert_eq!(line_range(text, &(3..3)), 0..5); // empty selection: whole document
        assert_eq!(line_range(text, &(1..3)), 0..3); // "b\na" from a partial selection
        assert_eq!(line_range(text, &(0..4)), 0..3); // ends at the start of "c"
        assert_eq!(line_range("x\r\ny\r\n", &(0..0)), 0..4);
        assert_eq!(line_range("no newline", &(0..0)), 0..10);
    }

    #[test]
    fn line_commands_keep_crlf() {
        assert_eq!(sort_lines("b\r\na\r\nc"), "a\r\nb\r\nc");
        assert_eq!(unique_lines("a\nb\na\nb\nc"), "a\nb\nc");
        assert_eq!(reverse_lines("1\n2\n3"), "3\n2\n1");
        assert_eq!(trim_trailing("a  \nb\t\n c "), "a\nb\n c");
    }

    #[test]
    fn json_keeps_key_order_and_number_text() {
        let source = r#"{"b":1,"a":1.10,"n":123456789012345678901234567890,"s":"é"}"#;
        assert_eq!(
            json_format(source).unwrap(),
            "{\n  \"b\": 1,\n  \"a\": 1.10,\n  \"n\": 123456789012345678901234567890,\n  \"s\": \"é\"\n}"
        );
        assert_eq!(json_minify(&json_format(source).unwrap()).unwrap(), source);
        assert!(json_format("{,}").unwrap_err().contains("not valid JSON"));
        assert_eq!(json_format_indented("[1]", "\t").unwrap(), "[\n\t1\n]");
        assert_eq!(json_format_indented("{}", "    ").unwrap(), "{}");
        assert_eq!(trimmed("  {}\n", 0..5), 2..4);
    }

    #[test]
    fn case_conversion_splits_identifiers() {
        let text = "parseHTTPResponse user_id\nkebab-case-name";
        assert_eq!(
            convert_case(text, Case::Snake),
            "parse_http_response user_id\nkebab_case_name"
        );
        assert_eq!(
            convert_case(text, Case::Camel),
            "parseHttpResponse userId\nkebabCaseName"
        );
        assert_eq!(convert_case("user_id", Case::Pascal), "UserId");
        assert_eq!(convert_case("userId", Case::Constant), "USER_ID");
        assert_eq!(convert_case("UserID", Case::Kebab), "user-id");
        assert_eq!(convert_case("version2Beta", Case::Snake), "version2_beta");
        assert_eq!(convert_case("hello wORLD", Case::Title), "Hello World");
        assert_eq!(convert_case("Straße", Case::Upper), "STRASSE");
    }

    #[test]
    fn encodings_round_trip_and_reject_bad_input() {
        for text in ["", "f", "fo", "foo", "foob", "日本語 ✓"] {
            assert_eq!(base64_decode(&base64_encode(text)).unwrap(), text);
            assert_eq!(url_decode(&url_encode(text)).unwrap(), text);
        }
        assert_eq!(base64_encode("hello"), "aGVsbG8=");
        assert_eq!(url_encode("a b&c/é"), "a%20b%26c%2F%C3%A9");
        assert!(base64_decode("abc").is_err());
        assert!(base64_decode("/w==").unwrap_err().contains("UTF-8"));
        assert!(url_decode("%zz").is_err());
        assert!(url_decode("%FF").unwrap_err().contains("UTF-8"));
    }

    #[test]
    fn count_reports_lines_words_and_characters() {
        assert_eq!(
            count("one two\nthree 猫\n"),
            "2 lines, 4 words, 16 characters"
        );
        assert_eq!(count(""), "0 lines, 0 words, 0 characters");
    }
}
