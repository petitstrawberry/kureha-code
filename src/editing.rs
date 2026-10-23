//! Pure code-editing commands over Kureha's LF-normalized UTF-8 text.
//!
//! Every plan contains one replacement in the **original** document and the
//! selection in the **resulting** document. Apply both as one undo transaction,
//! recording the final selection in the undo engine as well as in ScarletUI.
//! No command mutates a document or accesses the filesystem.
//!
//! Invalid byte offsets (including offsets inside a UTF-8 character), an
//! unsupported comment language, and commands that cannot change anything return
//! `None`. Nonempty selections ending at the start of a line exclude that line.
//! Anchor/caret direction is retained except by `newline`, which collapses it.

use scarlet_ui::prelude::{EditDelta, TextPosition, TextSelection};
use std::ops::Range;

pub const INDENT_WIDTH: usize = 4;
const INDENT: &str = "    ";

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EditPlan {
    /// Ordered disjoint source edits; empty means use the single envelope.
    pub parts: Vec<EditDelta>,
    pub edit: EditDelta,
    pub selection: TextSelection,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Direction {
    Up,
    Down,
}

/// A bare file extension, optionally prefixed with a dot; matching ignores case.
/// Unknown extensions deliberately have no guessed comment syntax.
pub fn line_comment_prefix(extension: &str) -> Option<&'static str> {
    let extension = extension.strip_prefix('.').unwrap_or(extension);
    if [
        "rs", "c", "h", "cc", "cpp", "cxx", "hh", "hpp", "hxx", "js", "jsx", "mjs", "cjs", "ts",
        "tsx", "mts", "cts",
    ]
    .iter()
    .any(|known| extension.eq_ignore_ascii_case(known))
    {
        Some("//")
    } else if ["py", "pyw", "sh", "bash", "zsh", "fish"]
        .iter()
        .any(|known| extension.eq_ignore_ascii_case(known))
    {
        Some("#")
    } else {
        None
    }
}

/// Toggle comments after each line's existing indentation. If every nonblank
/// line is commented, remove one prefix and at most one following space;
/// otherwise add a prefix and a space. Blank lines in a multiline selection are
/// left alone. A single blank line can be commented to start writing a comment.
pub fn toggle_line_comment(
    text: &str,
    selection: TextSelection,
    extension: &str,
) -> Option<EditPlan> {
    let prefix = line_comment_prefix(extension)?;
    let block = selected_lines(text, selection)?;
    let single_line = !text[block.clone()].contains('\n');
    let lines: Vec<_> = line_ranges(text, &block)
        .map(|range| {
            let indent = indentation_len(&text[range.clone()]);
            range.start + indent..range.end
        })
        .filter(|range| !range.is_empty() || single_line)
        .collect();
    let uncomment = !lines.is_empty()
        && lines
            .iter()
            .all(|range| text[range.clone()].starts_with(prefix));
    let changes = lines
        .into_iter()
        .map(|range| {
            if uncomment {
                let mut end = range.start + prefix.len();
                if text.as_bytes().get(end) == Some(&b' ') {
                    end += 1;
                }
                EditDelta {
                    replaced_range: range.start..end,
                    inserted: String::new(),
                }
            } else {
                EditDelta {
                    replaced_range: range.start..range.start,
                    inserted: format!("{prefix} "),
                }
            }
        })
        .collect();
    line_changes(text, selection, block.start, changes)
}

/// Add four spaces to every touched line (or to the current line for a caret).
/// Existing tabs are preserved; this command never converts the rest of a line.
pub fn indent(text: &str, selection: TextSelection) -> Option<EditPlan> {
    let block = selected_lines(text, selection)?;
    let changes = line_ranges(text, &block)
        .map(|range| EditDelta {
            replaced_range: range.start..range.start,
            inserted: INDENT.into(),
        })
        .collect();
    line_changes(text, selection, block.start, changes)
}

/// Remove up to one four-column indentation level. A tab advances to the next
/// four-column tab stop, so mixed prefixes such as two spaces plus a tab lose
/// one level together, without deleting any non-whitespace text.
pub fn outdent(text: &str, selection: TextSelection) -> Option<EditPlan> {
    let block = selected_lines(text, selection)?;
    let changes = line_ranges(text, &block)
        .filter_map(|range| {
            let mut bytes = 0;
            let mut columns = 0;
            for byte in text[range.clone()].bytes() {
                if columns >= INDENT_WIDTH {
                    break;
                }
                match byte {
                    b' ' => columns += 1,
                    b'\t' => columns += INDENT_WIDTH - columns % INDENT_WIDTH,
                    _ => break,
                }
                bytes += 1;
            }
            (bytes > 0).then(|| EditDelta {
                replaced_range: range.start..range.start + bytes,
                inserted: String::new(),
            })
        })
        .collect();
    line_changes(text, selection, block.start, changes)
}

/// Copy the exact selected bytes immediately after the selection and select the
/// copy, retaining its direction. With only a caret, duplicate its line below.
pub fn duplicate_selection(text: &str, selection: TextSelection) -> Option<EditPlan> {
    let range = selection_range(text, selection)?;
    if range.is_empty() {
        return duplicate_lines(text, selection, Direction::Down);
    }
    let end = range.end;
    Some(EditPlan {
        parts: Vec::new(),
        edit: EditDelta {
            replaced_range: end..end,
            inserted: text[range.clone()].into(),
        },
        selection: directed_selection(selection, end, end + range.len()),
    })
}

/// Copy complete touched lines above or below, moving the selection/caret into
/// the new copy at the same byte columns. The final logical empty line of a
/// newline-terminated file is a real line, just as it is in TextDocument.
pub fn duplicate_lines(
    text: &str,
    selection: TextSelection,
    direction: Direction,
) -> Option<EditPlan> {
    let block = selected_lines(text, selection)?;
    let content = &text[block.clone()];
    let (at, inserted, new_start) = match direction {
        Direction::Up => (block.start, format!("{content}\n"), block.start),
        Direction::Down => (block.end, format!("\n{content}"), block.end + 1),
    };
    let new_len = text.len() + inserted.len();
    Some(EditPlan {
        parts: Vec::new(),
        edit: EditDelta {
            replaced_range: at..at,
            inserted,
        },
        selection: relocate_selection(selection, block.start, new_start, new_len),
    })
}

/// Swap the touched lines with one adjacent logical line. Only the affected
/// block is replaced; newline separators outside it remain untouched. At EOF,
/// an endpoint after a selected newline clamps to the moved block's end if its
/// new last line has no terminator. Moving beyond the first/last line is a no-op.
pub fn move_lines(text: &str, selection: TextSelection, direction: Direction) -> Option<EditPlan> {
    let block = selected_lines(text, selection)?;
    let content = &text[block.clone()];
    let (range, inserted, new_start) = match direction {
        Direction::Up => {
            if block.start == 0 {
                return None;
            }
            let previous_start = line_start(text, block.start - 1);
            let previous = &text[previous_start..block.start - 1];
            (
                previous_start..block.end,
                format!("{content}\n{previous}"),
                previous_start,
            )
        }
        Direction::Down => {
            if block.end == text.len() {
                return None;
            }
            let next_end = line_end(text, block.end + 1);
            let next = &text[block.end + 1..next_end];
            (
                block.start..next_end,
                format!("{next}\n{content}"),
                block.start + next.len() + 1,
            )
        }
    };
    Some(EditPlan {
        parts: Vec::new(),
        edit: EditDelta {
            replaced_range: range,
            inserted,
        },
        selection: relocate_selection(selection, block.start, new_start, text.len()),
    })
}

/// Replace the selection with a newline and the leading whitespace retained
/// before its start. With a caret between a same-line `{ ... }` pair containing
/// only horizontal whitespace, insert an indented inner line and put the closing
/// brace on its own line. A conservative lexical check avoids that expansion in
/// comments and common quoted/raw strings; this is not a language parser.
pub fn newline(text: &str, selection: TextSelection) -> Option<EditPlan> {
    let range = selection_range(text, selection)?;
    let start = line_start(text, range.start);
    let before = &text[start..range.start];
    let indentation = &before[..indentation_len(before)];
    if range.is_empty() {
        let left = before.trim_end_matches([' ', '\t']);
        let after = &text[range.end..line_end(text, range.end)];
        let right = after.trim_start_matches([' ', '\t']);
        if left.ends_with('{') && right.starts_with('}') {
            let open = start + left.len() - 1;
            // `${}` and escaped braces need language-specific handling. Keep
            // plain newline behavior for those ambiguous cases.
            if !matches!(
                open.checked_sub(1).map(|i| text.as_bytes()[i]),
                Some(b'$' | b'\\')
            ) && code_at(text, open)
            {
                let close = range.end + after.len() - right.len();
                let inserted = format!("\n{indentation}{INDENT}\n{indentation}");
                return Some(EditPlan {
                    parts: Vec::new(),
                    selection: TextSelection::collapsed(
                        open + 2 + indentation.len() + INDENT.len(),
                    ),
                    edit: EditDelta {
                        replaced_range: open + 1..close,
                        inserted,
                    },
                });
            }
        }
    }
    let inserted = format!("\n{indentation}");
    Some(EditPlan {
        parts: Vec::new(),
        selection: TextSelection::collapsed(range.start + inserted.len()),
        edit: EditDelta {
            replaced_range: range,
            inserted,
        },
    })
}

fn selection_range(text: &str, selection: TextSelection) -> Option<Range<usize>> {
    let start = selection.start().byte;
    let end = selection.end().byte;
    (end <= text.len() && text.is_char_boundary(start) && text.is_char_boundary(end))
        .then_some(start..end)
}

fn line_start(text: &str, byte: usize) -> usize {
    text[..byte].rfind('\n').map_or(0, |at| at + 1)
}

fn line_end(text: &str, byte: usize) -> usize {
    text[byte..].find('\n').map_or(text.len(), |at| byte + at)
}

/// Includes internal newlines, but excludes the last touched line's terminator.
fn selected_lines(text: &str, selection: TextSelection) -> Option<Range<usize>> {
    let range = selection_range(text, selection)?;
    let last = if !range.is_empty() && text.as_bytes()[range.end - 1] == b'\n' {
        range.end - 1
    } else {
        range.end
    };
    Some(line_start(text, range.start)..line_end(text, last))
}

fn line_ranges<'a>(
    text: &'a str,
    block: &'a Range<usize>,
) -> impl Iterator<Item = Range<usize>> + 'a {
    let mut next = Some(block.start);
    std::iter::from_fn(move || {
        let start = next?;
        let end = line_end(text, start);
        next = (end < block.end).then_some(end + 1);
        Some(start..end)
    })
}

fn indentation_len(text: &str) -> usize {
    text.bytes()
        .take_while(|byte| matches!(byte, b' ' | b'\t'))
        .count()
}

fn directed_selection(original: TextSelection, start: usize, end: usize) -> TextSelection {
    let (anchor, caret) = if original.anchor <= original.caret {
        (start, end)
    } else {
        (end, start)
    };
    TextSelection {
        anchor: TextPosition::new(anchor),
        caret: TextPosition::new(caret),
    }
}

fn relocate_selection(
    selection: TextSelection,
    old_start: usize,
    new_start: usize,
    new_len: usize,
) -> TextSelection {
    let relocate = |position: TextPosition| {
        TextPosition::new((new_start + position.byte - old_start).min(new_len))
    };
    TextSelection {
        anchor: relocate(selection.anchor),
        caret: relocate(selection.caret),
    }
}

/// Compose disjoint, ordered prefix changes into a single transaction while
/// mapping endpoints through the individual changes rather than their envelope.
fn line_changes(
    text: &str,
    selection: TextSelection,
    first_line_start: usize,
    changes: Vec<EditDelta>,
) -> Option<EditPlan> {
    let range = changes.first()?.replaced_range.start..changes.last()?.replaced_range.end;
    let mut inserted = String::new();
    let mut previous = range.start;
    for change in &changes {
        inserted.push_str(&text[previous..change.replaced_range.start]);
        inserted.push_str(&change.inserted);
        previous = change.replaced_range.end;
    }
    let map = |position: TextPosition| {
        // Keep a full-line selection's leading edge at column zero. For a caret
        // or a partial selection, follow the original content after insertion.
        let left = !selection.is_collapsed() && position.byte == first_line_start;
        let mut removed = 0;
        let mut added = 0;
        for change in &changes {
            let range = &change.replaced_range;
            if position.byte < range.start || (left && position.byte == range.start) {
                break;
            }
            if position.byte <= range.end {
                return TextPosition::new(range.start - removed + added + change.inserted.len());
            }
            removed += range.len();
            added += change.inserted.len();
        }
        TextPosition::new(position.byte - removed + added)
    };
    Some(EditPlan {
        parts: changes.clone(),
        edit: EditDelta {
            replaced_range: range,
            inserted,
        },
        selection: TextSelection {
            anchor: map(selection.anchor),
            caret: map(selection.caret),
        },
    })
}

/// A conservative cross-language guard, used only for the optional brace-pair
/// expansion. Unknown/unclosed strings suppress expansion instead of guessing.
fn code_at(text: &str, target: usize) -> bool {
    let bytes = text.as_bytes();
    let mut at = 0;
    while at < target {
        if bytes[at..].starts_with(b"//") || bytes[at] == b'#' {
            at = bytes[at..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |i| at + i + 1);
        } else if bytes[at..].starts_with(b"/*") {
            let mut depth = 1;
            at += 2;
            while at < bytes.len() && depth > 0 {
                if bytes[at..].starts_with(b"/*") {
                    depth += 1;
                    at += 2;
                } else if bytes[at..].starts_with(b"*/") {
                    depth -= 1;
                    at += 2;
                } else {
                    at += 1;
                }
            }
        } else if bytes[at] == b'/' {
            // Without a language parser a slash may start a JavaScript regexp.
            // Suppress pair expansion on the rest of this line, even if this
            // particular slash was division; normal newline still works.
            at = bytes[at..]
                .iter()
                .position(|&b| b == b'\n')
                .map_or(bytes.len(), |i| at + i + 1);
        } else if let Some((body, delimiter)) = raw_string_start(bytes, at) {
            at = find_bytes(&bytes[body..], &delimiter)
                .map_or(bytes.len(), |i| body + i + delimiter.len());
        } else if matches!(bytes[at], b'\'' | b'"' | b'`') {
            let quote = bytes[at];
            let width = if quote != b'`' && bytes[at..].starts_with(&[quote; 3]) {
                3
            } else {
                1
            };
            at += width;
            while at < bytes.len() {
                if bytes[at] == b'\\' {
                    at = (at + 2).min(bytes.len());
                } else if bytes[at..].starts_with(&[quote; 3][..width]) {
                    at += width;
                    break;
                } else {
                    at += 1;
                }
            }
        } else {
            at += 1;
        }
    }
    at == target
}

fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Rust raw strings (including br/cr prefixes) and C++ R"delimiter(...)delimiter".
fn raw_string_start(bytes: &[u8], at: usize) -> Option<(usize, Vec<u8>)> {
    if bytes[at] == b'r' {
        let mut end = at + 1;
        while bytes.get(end) == Some(&b'#') {
            end += 1;
        }
        if bytes.get(end) == Some(&b'"') {
            let mut delimiter = vec![b'"'];
            delimiter.extend_from_slice(&bytes[at + 1..end]);
            return Some((end + 1, delimiter));
        }
    } else if bytes[at..].starts_with(b"R\"") {
        let start = at + 2;
        let length = bytes[start..].iter().take(17).position(|&b| b == b'(')?;
        let tag = &bytes[start..start + length];
        if tag
            .iter()
            .all(|&b| !b.is_ascii_whitespace() && !matches!(b, b'\\' | b')'))
        {
            let mut delimiter = vec![b')'];
            delimiter.extend_from_slice(tag);
            delimiter.push(b'"');
            return Some((start + length + 1, delimiter));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use scarlet_ui::prelude::TextDocument;

    fn selection(anchor: usize, caret: usize) -> TextSelection {
        TextSelection {
            anchor: TextPosition::new(anchor),
            caret: TextPosition::new(caret),
        }
    }

    fn apply(text: &str, plan: EditPlan) -> (String, TextSelection) {
        assert!(text.is_char_boundary(plan.edit.replaced_range.start));
        assert!(text.is_char_boundary(plan.edit.replaced_range.end));
        let (document, delta) = TextDocument::from_str(text)
            .replace(plan.edit.replaced_range.clone(), &plan.edit.inserted);
        assert_eq!(delta, plan.edit);
        let result = document.as_str().into_owned();
        assert!(result.is_char_boundary(plan.selection.anchor.byte));
        assert!(result.is_char_boundary(plan.selection.caret.byte));
        (result, plan.selection)
    }

    #[test]
    fn comments_use_known_extensions_only() {
        for extension in [
            "rs", "C", ".cpp", "h", "js", "jsx", "ts", "tsx", "mjs", "cts",
        ] {
            assert_eq!(line_comment_prefix(extension), Some("//"));
        }
        for extension in ["py", ".SH", "bash", "zsh", "fish"] {
            assert_eq!(line_comment_prefix(extension), Some("#"));
        }
        for extension in ["", "json", "html", "css", "txt", "example.rs"] {
            assert_eq!(line_comment_prefix(extension), None);
            assert!(toggle_line_comment("text", selection(0, 4), extension).is_none());
        }
    }

    #[test]
    fn comments_preserve_indentation_unicode_direction_and_exclusive_end() {
        let text = "  猫();\n\t🍁();\nnext();\n";
        let end = text.find("next").unwrap();
        let original = selection(end, 0);
        let (commented, after) = apply(text, toggle_line_comment(text, original, "rs").unwrap());
        assert_eq!(commented, "  // 猫();\n\t// 🍁();\nnext();\n");
        assert_eq!(after, selection(end + 6, 0));
        assert_eq!(
            apply(
                &commented,
                toggle_line_comment(&commented, after, "rs").unwrap()
            ),
            (text.into(), original)
        );
    }

    #[test]
    fn mixed_comments_add_a_layer_and_blank_lines_are_unchanged() {
        let text = "  // one\n \t\n  two\n";
        let sel = selection(0, text.len());
        let (changed, after) = apply(text, toggle_line_comment(text, sel, "ts").unwrap());
        assert_eq!(changed, "  // // one\n \t\n  // two\n");
        assert_eq!(
            apply(
                &changed,
                toggle_line_comment(&changed, after, "ts").unwrap()
            ),
            (text.into(), sel)
        );
        assert!(toggle_line_comment(" \n\t\n", selection(0, 4), "py").is_none());
    }

    #[test]
    fn comment_removes_only_one_optional_space_and_maps_caret_inside_marker() {
        let text = "  #  value";
        assert_eq!(
            apply(
                text,
                toggle_line_comment(text, TextSelection::collapsed(3), "py").unwrap()
            ),
            ("   value".into(), TextSelection::collapsed(2))
        );
        assert_eq!(
            apply(
                "#value",
                toggle_line_comment("#value", TextSelection::collapsed(6), "py").unwrap()
            ),
            ("value".into(), TextSelection::collapsed(5))
        );
        assert_eq!(
            apply(
                "\t",
                toggle_line_comment("\t", TextSelection::collapsed(1), "py").unwrap()
            ),
            ("\t# ".into(), TextSelection::collapsed(3))
        );
    }

    #[test]
    fn indent_round_trip_preserves_reversed_full_line_selection() {
        let text = "猫\n\t🍁\nlast";
        let end = text.find("last").unwrap();
        let sel = selection(end, 0);
        let (indented, after) = apply(text, indent(text, sel).unwrap());
        assert_eq!(indented, "    猫\n    \t🍁\nlast");
        assert_eq!(after, selection(end + 8, 0));
        assert_eq!(
            apply(&indented, outdent(&indented, after).unwrap()),
            (text.into(), sel)
        );
    }

    #[test]
    fn indent_partial_selection_tracks_original_characters() {
        let text = "a猫b\nc🍁d";
        let sel = selection(1, text.len() - 1);
        let (changed, after) = apply(text, indent(text, sel).unwrap());
        assert_eq!(after, selection(5, text.len() + 7));
        assert_eq!(
            apply(&changed, outdent(&changed, after).unwrap()),
            (text.into(), sel)
        );
        assert_eq!(
            apply("\t猫", indent("\t猫", TextSelection::collapsed(1)).unwrap()),
            ("    \t猫".into(), TextSelection::collapsed(5))
        );
    }

    #[test]
    fn outdent_handles_mixed_tabs_short_indents_and_caret_clamping() {
        let text = "  \t猫\n\t  🍁\n x\nno";
        let (result, after) = apply(text, outdent(text, selection(text.len(), 1)).unwrap());
        assert_eq!(result, "猫\n  🍁\nx\nno");
        assert_eq!(after, selection(result.len(), 0));
        assert_eq!(
            apply("  x", outdent("  x", TextSelection::collapsed(1)).unwrap()),
            ("x".into(), TextSelection::collapsed(0))
        );
        assert!(outdent("猫\nno", selection(0, 6)).is_none());
    }

    #[test]
    fn end_at_next_line_start_excludes_it_but_collapsed_caret_includes_it() {
        assert_eq!(
            apply("a\nb", indent("a\nb", selection(0, 2)).unwrap()).0,
            "    a\nb"
        );
        assert_eq!(
            apply("a\nb", indent("a\nb", TextSelection::collapsed(2)).unwrap()).0,
            "a\n    b"
        );
        assert_eq!(
            apply("a\n", indent("a\n", selection(0, 2)).unwrap()).0,
            "    a\n"
        );
        assert_eq!(
            apply("a\n", indent("a\n", TextSelection::collapsed(2)).unwrap()).0,
            "a\n    "
        );
    }

    #[test]
    fn exact_duplication_selects_new_copy_with_original_direction() {
        let text = "a猫🍁z";
        let range = 1..text.len() - 1;
        let (result, after) = apply(
            text,
            duplicate_selection(text, selection(range.end, range.start)).unwrap(),
        );
        assert_eq!(result, "a猫🍁猫🍁z");
        assert_eq!(after, selection(range.end + range.len(), range.end));
        assert_eq!(
            apply(
                "a\nb\nc",
                duplicate_selection("a\nb\nc", selection(1, 4)).unwrap()
            )
            .0,
            "a\nb\n\nb\nc"
        );
    }

    #[test]
    fn duplicate_lines_handles_eof_terminators_and_empty_documents() {
        for text in ["猫", "猫\n"] {
            let sel = TextSelection::collapsed(3);
            assert_eq!(
                apply(text, duplicate_lines(text, sel, Direction::Down).unwrap()),
                (format!("猫\n{text}"), TextSelection::collapsed(7))
            );
            assert_eq!(
                apply(text, duplicate_lines(text, sel, Direction::Up).unwrap()),
                (format!("猫\n{text}"), sel)
            );
        }
        assert_eq!(
            apply(
                "",
                duplicate_selection("", TextSelection::default()).unwrap()
            ),
            ("\n".into(), TextSelection::collapsed(1))
        );
        assert_eq!(
            apply(
                "a\n",
                duplicate_lines("a\n", TextSelection::collapsed(2), Direction::Down).unwrap()
            ),
            ("a\n\n".into(), TextSelection::collapsed(3))
        );
    }

    #[test]
    fn duplicate_line_blocks_selects_the_copy_excluding_next_line() {
        let text = "a\n猫\nz";
        let (result, after) = apply(
            text,
            duplicate_lines(text, selection(6, 0), Direction::Down).unwrap(),
        );
        assert_eq!(result, "a\n猫\na\n猫\nz");
        assert_eq!(after, selection(12, 6));
        let (result, after) = apply(
            "a\n猫",
            duplicate_lines("a\n猫", selection(5, 0), Direction::Down).unwrap(),
        );
        assert_eq!(result, "a\n猫\na\n猫");
        assert_eq!(after, selection(11, 6));
    }

    #[test]
    fn move_block_preserves_unicode_columns_and_reversed_direction() {
        let text = "top\n  猫\n\t🍁\nbottom\n";
        let start = text.find('猫').unwrap();
        let end = text.find("bottom").unwrap();
        let sel = selection(end, start);
        let (moved, after) = apply(text, move_lines(text, sel, Direction::Up).unwrap());
        assert_eq!(moved, "  猫\n\t🍁\ntop\nbottom\n");
        assert_eq!(after, selection(end - 4, start - 4));
        assert_eq!(
            apply(&moved, move_lines(&moved, after, Direction::Down).unwrap()),
            (text.into(), sel)
        );
    }

    #[test]
    fn move_down_to_unterminated_eof_clamps_exclusive_newline_endpoint() {
        let (result, after) = apply(
            "猫\nz",
            move_lines("猫\nz", selection(4, 0), Direction::Down).unwrap(),
        );
        assert_eq!(result, "z\n猫");
        assert_eq!(after, selection(5, 2));
        assert_eq!(
            apply(
                "a\n猫\n",
                move_lines("a\n猫\n", selection(0, 2), Direction::Down).unwrap()
            ),
            ("猫\na\n".into(), selection(4, 6))
        );
    }

    #[test]
    fn moving_final_empty_logical_line_is_well_defined() {
        assert_eq!(
            apply(
                "a\n",
                move_lines("a\n", TextSelection::collapsed(2), Direction::Up).unwrap()
            ),
            ("\na".into(), TextSelection::collapsed(0))
        );
        assert_eq!(
            apply(
                "a\n",
                move_lines("a\n", TextSelection::collapsed(0), Direction::Down).unwrap()
            ),
            ("\na".into(), TextSelection::collapsed(1))
        );
        for text in ["", "a", "猫\n🍁"] {
            assert!(move_lines(text, TextSelection::default(), Direction::Up).is_none());
            assert!(
                move_lines(text, TextSelection::collapsed(text.len()), Direction::Down).is_none()
            );
            assert!(move_lines(text, selection(0, text.len()), Direction::Up).is_none());
        }
    }

    #[test]
    fn newline_retains_tabs_spaces_and_replaces_reversed_selection() {
        let text = "\t  猫🍁";
        assert_eq!(
            apply(text, newline(text, selection(text.len(), 6)).unwrap()),
            ("\t  猫\n\t  ".into(), TextSelection::collapsed(10))
        );
        assert_eq!(
            apply(
                "    x",
                newline("    x", TextSelection::collapsed(2)).unwrap()
            ),
            ("  \n    x".into(), TextSelection::collapsed(5))
        );
        assert_eq!(
            apply("", newline("", TextSelection::default()).unwrap()),
            ("\n".into(), TextSelection::collapsed(1))
        );
    }

    #[test]
    fn newline_expands_braces_and_consumes_only_inner_whitespace() {
        let text = "\tif 猫 {  \t } // tail\nnext";
        let caret = text.find("  \t").unwrap() + 1;
        let (result, after) = apply(
            text,
            newline(text, TextSelection::collapsed(caret)).unwrap(),
        );
        assert_eq!(result, "\tif 猫 {\n\t    \n\t} // tail\nnext");
        assert_eq!(
            after,
            TextSelection::collapsed(result.find("\n\t}").unwrap())
        );
        assert_eq!(
            apply("{}", newline("{}", TextSelection::collapsed(1)).unwrap()),
            ("{\n    \n}".into(), TextSelection::collapsed(6))
        );
    }

    #[test]
    fn newline_does_not_expand_selected_content_or_nonempty_brace_pairs() {
        assert_eq!(
            apply("{abc}", newline("{abc}", selection(4, 1)).unwrap()),
            ("{\n}".into(), TextSelection::collapsed(2))
        );
        assert_eq!(
            apply(
                "{abc}",
                newline("{abc}", TextSelection::collapsed(1)).unwrap()
            )
            .0,
            "{\nabc}"
        );
    }

    #[test]
    fn brace_expansion_avoids_comments_and_quoted_multiline_raw_strings() {
        for text in [
            "// {}",
            "# {}",
            "/* {} */",
            "/* outer /* inner */\n {} */",
            "let s = \"{}\";",
            "'{}'",
            "`line\n{} ${x}`",
            "\"\"\"line\n{}\"\"\"",
            "r#\"line \"\n{}\"#",
            "br##\"\n{}\"##",
            "R\"tag(\"\n{})tag\"",
            "return /{}/;",
            "${}",
            "\\{}",
        ] {
            let caret = text.find("{}").unwrap() + 1;
            let plan = newline(text, TextSelection::collapsed(caret)).unwrap();
            assert_eq!(plan.edit.replaced_range, caret..caret, "{text:?}");
            assert_eq!(plan.edit.inserted.matches('\n').count(), 1, "{text:?}");
        }
        for text in [
            "let s = \"escaped \\\"\"; {}",
            "/* closed */ {}",
            "// comment\n{}",
            "r#\"raw\"#; {}",
            "R\"x(raw)x\"; {}",
            "let ratio = a / b;\n{}",
        ] {
            let caret = text.rfind("{}").unwrap() + 1;
            assert_eq!(
                newline(text, TextSelection::collapsed(caret))
                    .unwrap()
                    .edit
                    .inserted,
                "\n    \n",
                "{text:?}"
            );
        }
    }

    #[test]
    fn invalid_utf8_offsets_are_rejected_by_all_commands() {
        let text = "猫🍁";
        for sel in [
            selection(1, 3),
            selection(0, 4),
            selection(0, 99),
            selection(usize::MAX, 0),
        ] {
            assert!(toggle_line_comment(text, sel, "rs").is_none());
            assert!(indent(text, sel).is_none());
            assert!(outdent(text, sel).is_none());
            assert!(duplicate_selection(text, sel).is_none());
            assert!(newline(text, sel).is_none());
            for direction in [Direction::Up, Direction::Down] {
                assert!(move_lines(text, sel, direction).is_none());
                assert!(duplicate_lines(text, sel, direction).is_none());
            }
        }
    }

    #[test]
    fn line_operations_match_logical_line_model_for_every_selection() {
        // This reference operates on logical lines, independently of the byte
        // splice planner. In particular, split preserves the empty EOF line.
        for text in ["", "\n", "a\n猫\n🍁", "a\n猫\n🍁\n", "\n\n猫\n\n"] {
            let boundaries: Vec<_> = (0..=text.len())
                .filter(|&byte| text.is_char_boundary(byte))
                .collect();
            for &anchor in &boundaries {
                for &caret in &boundaries {
                    let sel = selection(anchor, caret);
                    let first = text[..anchor.min(caret)].matches('\n').count();
                    let end = anchor.max(caret);
                    let mut last = text[..end].matches('\n').count();
                    if anchor != caret && text[..end].ends_with('\n') {
                        last -= 1;
                    }
                    for direction in [Direction::Up, Direction::Down] {
                        let mut lines: Vec<_> = text.split('\n').collect();
                        let copy = lines[first..=last].to_vec();
                        let at = match direction {
                            Direction::Up => first,
                            Direction::Down => last + 1,
                        };
                        lines.splice(at..at, copy);
                        assert_eq!(
                            apply(text, duplicate_lines(text, sel, direction).unwrap()).0,
                            lines.join("\n"),
                            "duplicate {text:?} {sel:?} {direction:?}"
                        );

                        let mut lines: Vec<_> = text.split('\n').collect();
                        let can_move = match direction {
                            Direction::Up if first > 0 => {
                                lines[first - 1..=last].rotate_left(1);
                                true
                            }
                            Direction::Down if last + 1 < lines.len() => {
                                lines[first..=last + 1].rotate_right(1);
                                true
                            }
                            _ => false,
                        };
                        let plan = move_lines(text, sel, direction);
                        if can_move {
                            assert_eq!(
                                apply(text, plan.unwrap()).0,
                                lines.join("\n"),
                                "move {text:?} {sel:?} {direction:?}"
                            );
                        } else {
                            assert!(plan.is_none());
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn all_valid_small_unicode_selections_produce_valid_transactions() {
        for text in [
            "",
            "\n",
            "猫\n🍁\n",
            " a\n\t猫",
            "e\u{301}\n👩\u{200d}💻",
            " {}\n  x\n",
        ] {
            let boundaries: Vec<_> = (0..=text.len())
                .filter(|&byte| text.is_char_boundary(byte))
                .collect();
            for &anchor in &boundaries {
                for &caret in &boundaries {
                    let sel = selection(anchor, caret);
                    let mut plans = vec![
                        indent(text, sel),
                        outdent(text, sel),
                        toggle_line_comment(text, sel, "rs"),
                        duplicate_selection(text, sel),
                        newline(text, sel),
                    ];
                    for direction in [Direction::Up, Direction::Down] {
                        plans.push(move_lines(text, sel, direction));
                        plans.push(duplicate_lines(text, sel, direction));
                    }
                    for plan in plans.into_iter().flatten() {
                        let (result, _) = apply(text, plan.clone());
                        let restored = TextDocument::from_str(&result)
                            .replace(
                                plan.edit.replaced_range.start
                                    ..plan.edit.replaced_range.start + plan.edit.inserted.len(),
                                &text[plan.edit.replaced_range.clone()],
                            )
                            .0;
                        assert_eq!(restored.as_str(), text);
                    }
                }
            }
        }
    }
}
