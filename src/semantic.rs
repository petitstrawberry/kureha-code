//! Bounded LSP semantic tokens, mapped through the same theme as lexical colors.
use crate::theme::Theme;
use scarlet_ui::prelude::{Color, TextStyleSpan};
use serde_json::{Value, json};
use syntect::{highlighting::Highlighter, parsing::Scope};

pub fn capabilities() -> Value {
    // Request syntax tokens too: rust-analyzer otherwise omits unmodified
    // keywords and punctuation, preventing semantic bracket-pair coloring.
    json!({"dynamicRegistration":false,"requests":{"full":true},
        "tokenTypes":["namespace","type","class","enum","interface","struct","typeParameter",
            "parameter","variable","property","enumMember","event","function","method","macro",
            "keyword","modifier","comment","string","number","regexp","operator","decorator",
            "selfKeyword","selfTypeKeyword","lifetime","attribute","builtinType","constParameter",
            "brace","bracket","parenthesis","punctuation","colon","comma","dot","semicolon",
            "typeAlias","union","toolModule","procMacro","const","static","boolean",
            "builtinAttribute","derive","deriveHelper","label","unresolvedReference"],
        "tokenModifiers":["declaration","definition","readonly","static","deprecated","abstract",
            "async","modification","documentation","defaultLibrary"],
        "formats":["relative"],"overlappingTokenSupport":false,"multilineTokenSupport":false,
        "serverCancelSupport":true,"augmentsSyntaxTokens":false})
}

/// Validate the entire response before publishing any spans. Positions are UTF-16;
/// walking each line once avoids quadratic scans for large token responses.
pub fn decode(
    text: &str,
    result: &Value,
    legend: &Value,
    theme: &Theme,
) -> Option<Vec<TextStyleSpan>> {
    if text.len() > crate::syntax::MAX_BYTES {
        return None;
    }
    if result.is_null() {
        return Some(Vec::new());
    }
    let data = result.get("data")?.as_array()?;
    if data.len() % 5 != 0 || data.len() > 80_000 * 5 {
        return None;
    }
    let types = legend.get("tokenTypes")?.as_array()?;
    let modifiers = legend.get("tokenModifiers")?.as_array()?;
    let readonly = modifiers
        .iter()
        .position(|v| v == "readonly")
        .filter(|n| *n < 32);
    let declaration = modifiers
        .iter()
        .position(|v| v == "declaration")
        .filter(|n| *n < 32);
    let syntax = theme.syntect_theme();
    let highlighter = Highlighter::new(&syntax);
    let color = |scope: &str| {
        let stack: Vec<Scope> = scope
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        let c = highlighter.style_for_stack(&stack).foreground;
        Color::rgba(c.r, c.g, c.b, c.a)
    };
    let scopes: Vec<Option<Color>> = types
        .iter()
        .map(|t| {
            Some(color(match t.as_str()? {
                "namespace" | "toolModule" => "entity.name.namespace",
                "type" | "class" | "enum" | "interface" | "struct" | "typeParameter"
                | "selfTypeKeyword" | "typeAlias" | "union" => "entity.name.type",
                "builtinType" => "support.type",
                "function" | "method" | "macro" | "procMacro" => "entity.name.function",
                "enumMember" | "constParameter" | "const" | "static" | "boolean" => {
                    "constant.other"
                }
                "keyword" | "modifier" | "selfKeyword" | "builtinAttribute" => "keyword",
                "operator" | "colon" | "dot" => "keyword.operator",
                "parameter" => "variable.parameter",
                "variable" => "variable.other",
                "property" | "event" => "variable.other.member",
                "decorator" | "attribute" | "derive" | "deriveHelper" => {
                    "entity.name.function"
                }
                "lifetime" | "label" => "storage.modifier",
                // Keep richer lexical string/comment coloring (escapes, doc fences).
                "comment" | "string" | "regexp" | "number" => return None,
                "brace" | "bracket" | "parenthesis" | "punctuation" | "comma" | "semicolon" => {
                    "punctuation"
                }
                _ => return None,
            }))
        })
        .collect();
    let brackets = [
        color("punctuation.section.brackets.level1"),
        color("punctuation.section.brackets.level2"),
        color("punctuation.section.brackets.level3"),
    ];
    let constant = color("constant.other");
    let function = color("entity.name.function");
    let mut stack = Vec::new();
    let mut lines = vec![0];
    lines.extend(text.match_indices('\n').map(|(i, _)| i + 1));
    let mut line = 0usize;
    let mut character = 0usize;
    let mut mapped_line = None;
    let mut offsets = Vec::new();
    let mut end_of_previous = 0;
    let mut spans = Vec::new();
    for token in data.chunks_exact(5) {
        let mut n = [0usize; 5];
        for (out, value) in n.iter_mut().zip(token) {
            *out = usize::try_from(u32::try_from(value.as_u64()?).ok()?).ok()?;
        }
        line = line.checked_add(n[0])?;
        character = if n[0] == 0 {
            character.checked_add(n[1])?
        } else {
            n[1]
        };
        let start_line = *lines.get(line)?;
        let end_line = lines.get(line + 1).copied().unwrap_or(text.len());
        if mapped_line != Some(line) {
            offsets.clear();
            offsets.push(start_line);
            for (i, ch) in text[start_line..end_line].char_indices() {
                if ch == '\n'
                    || (ch == '\r' && text.as_bytes().get(start_line + i + 1) == Some(&b'\n'))
                {
                    break;
                }
                if ch.len_utf16() == 2 {
                    offsets.push(usize::MAX);
                }
                offsets.push(start_line + i + ch.len_utf8());
            }
            mapped_line = Some(line);
        }
        let start = *offsets.get(character)?;
        // LSP requires single-line clients to clamp a token at the line end.
        let end = offsets[character.checked_add(n[2])?.min(offsets.len() - 1)];
        if n[2] == 0
            || start == usize::MAX
            || end == usize::MAX
            || start < end_of_previous
            || end <= start
        {
            return None;
        }
        end_of_previous = end;
        let kind = types.get(n[3])?.as_str()?;
        let mut foreground = *scopes.get(n[3])?;
        if kind == "variable" && readonly.is_some_and(|bit| n[4] & (1 << bit) != 0) {
            foreground = Some(constant);
        }
        if kind == "namespace" && declaration.is_some_and(|bit| n[4] & (1 << bit) != 0) {
            foreground = Some(function);
        }
        if matches!(kind, "brace" | "bracket" | "parenthesis" | "punctuation") {
            let spelling = &text[start..end];
            if matches!(spelling, "{" | "[" | "(") {
                let c = brackets[stack.len() % brackets.len()];
                stack.push((spelling.as_bytes()[0], c));
                foreground = Some(c);
            } else if matches!(spelling, "}" | "]" | ")") {
                let open = match spelling {
                    "}" => b'{',
                    "]" => b'[',
                    _ => b'(',
                };
                // Pop each opener at most once, including malformed source.
                // Repeated unmatched closers must not rescan a deep stack.
                while let Some((candidate, color)) = stack.pop() {
                    if candidate == open {
                        foreground = Some(color);
                        break;
                    }
                }
            }
        }
        if let Some(color) = foreground {
            spans.push(TextStyleSpan {
                range: start..end,
                color,
            });
        }
    }
    Some(spans)
}

/// Semantic colors replace only their own ranges; lexical strings and comments
/// survive. Both inputs and the result are sorted, disjoint byte ranges.
pub(crate) fn overlay(lexical: &[TextStyleSpan], semantic: &[TextStyleSpan]) -> Vec<TextStyleSpan> {
    let mut result = Vec::new();
    let mut i = 0;
    let mut start = lexical.first().map_or(0, |s| s.range.start);
    for top in semantic {
        while let Some(base) = lexical.get(i) {
            if start >= top.range.start {
                break;
            }
            let end = base.range.end.min(top.range.start);
            if start < end {
                result.push(TextStyleSpan {
                    range: start..end,
                    color: base.color,
                });
            }
            if base.range.end > top.range.start {
                break;
            }
            i += 1;
            start = lexical.get(i).map_or(0, |s| s.range.start);
        }
        result.push(top.clone());
        while i < lexical.len() && lexical[i].range.end <= top.range.end {
            i += 1;
        }
        if let Some(base) = lexical.get(i) {
            start = base.range.start.max(top.range.end);
        }
    }
    while let Some(base) = lexical.get(i) {
        if start < base.range.end {
            result.push(TextStyleSpan {
                range: start..base.range.end,
                color: base.color,
            });
        }
        i += 1;
        start = lexical.get(i).map_or(0, |s| s.range.start);
    }
    result
}

#[cfg(test)]
#[path = "semantic_tests.rs"]
mod tests;
