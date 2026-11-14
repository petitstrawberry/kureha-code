//! Checkpoints live on the parser worker, never on the UI thread.
use super::*;
use std::collections::BTreeMap;
use syntect::{highlighting::HighlightState, parsing::ParseState};

type ParserState = (HighlightState, ParseState);

const CHECKPOINT_LINES: usize = 32;

struct Parsed {
    text: String,
    path: Option<PathBuf>,
    syntax: String,
    theme: EditorTheme,
    lines: Vec<Arc<[TextStyleSpan]>>,
    checkpoints: BTreeMap<usize, ParserState>,
    #[cfg(test)]
    parsed_lines: usize,
}

#[derive(Default)]
pub(super) struct WorkerCache(VecDeque<(u64, Parsed)>);

impl WorkerCache {
    pub fn highlight(&mut self, catalog: &Catalog, request: &Request) -> Highlight {
        let old = self
            .0
            .iter()
            .position(|(id, _)| *id == request.id)
            .and_then(|index| self.0.remove(index));
        let (highlight, parsed) = parse(catalog, request, old.as_ref().map(|(_, p)| p));
        if let Some(parsed) = parsed {
            self.0.push_back((request.id, parsed));
        } else if request.cancel.load(Ordering::Relaxed) {
            if let Some(old) = old {
                self.0.push_back(old);
            }
        }
        while self.0.len() > MAX_DOCUMENTS {
            self.0.pop_front();
        }
        highlight
    }
}

#[cfg(test)]
pub(super) fn full(catalog: &Catalog, request: &Request) -> Highlight {
    parse(catalog, request, None).0
}

fn parse(
    catalog: &Catalog,
    request: &Request,
    previous: Option<&Parsed>,
) -> (Highlight, Option<Parsed>) {
    let fail = |reason| (Highlight::plain(reason), None);
    if request.text.len() > MAX_BYTES {
        return fail("Syntax colors paused: document exceeds 1 MiB");
    }
    let lines: Vec<_> = LinesWithEndings::from(&request.text).collect();
    if lines.len() > MAX_LINES {
        return fail("Syntax colors paused: document exceeds 20,000 lines");
    }
    if lines.iter().any(|line| line.len() > MAX_LINE_BYTES) {
        return fail("Syntax colors paused: a line exceeds 16 KiB");
    }
    let syntax = catalog.syntax(request.key.path.as_deref(), &request.text);
    if syntax.name == "Plain Text" {
        return (Highlight::default(), None);
    }
    let previous = previous.filter(|old| {
        old.path == request.key.path && old.theme == request.theme && old.syntax == syntax.name
    });
    let old_lines: Vec<_> = previous
        .map(|old| LinesWithEndings::from(&old.text).collect())
        .unwrap_or_default();
    let prefix = lines
        .iter()
        .zip(&old_lines)
        .take_while(|(a, b)| a == b)
        .count();
    let suffix = lines[prefix..]
        .iter()
        .rev()
        .zip(old_lines[prefix..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    let new_suffix = lines.len() - suffix;
    let old_suffix = old_lines.len() - suffix;
    let resume = previous.and_then(|old| old.checkpoints.range(..=prefix).next_back());
    let start = resume.map_or(0, |(index, _)| *index);
    let theme = request.theme.syntect_theme();
    let neutral = theme.settings.foreground;
    let mut parser = match resume {
        Some((_, (h, p))) => HighlightLines::from_state(&theme, h.clone(), p.clone()),
        None => HighlightLines::new(syntax, &theme),
    };
    let mut parsed = Parsed {
        text: request.text.clone(),
        path: request.key.path.clone(),
        syntax: syntax.name.clone(),
        theme: request.theme.clone(),
        lines: Vec::with_capacity(lines.len()),
        checkpoints: BTreeMap::new(),
        #[cfg(test)]
        parsed_lines: 0,
    };
    if let Some(old) = previous {
        parsed.lines.extend_from_slice(&old.lines[..start]);
        parsed
            .checkpoints
            .extend(old.checkpoints.range(..start).map(|(i, s)| (*i, s.clone())));
    }
    let started = Instant::now();
    let mut span_count = parsed.lines.iter().map(|line| line.len()).sum::<usize>();
    for index in start..lines.len() {
        if request.cancel.load(Ordering::Relaxed) {
            return (Highlight::default(), None);
        }
        if started.elapsed() > PARSE_BUDGET {
            return fail("Syntax colors paused: highlighting budget exceeded");
        }
        let aligned = if index >= new_suffix && suffix > 0 {
            previous.and_then(|old| old.checkpoints.get(&(index - new_suffix + old_suffix)))
        } else {
            None
        };
        if index % CHECKPOINT_LINES == 0 || index == start || aligned.is_some() {
            let state = parser.state();
            if aligned == Some(&state) {
                let old = previous.unwrap();
                let old_index = index - new_suffix + old_suffix;
                parsed.lines.extend_from_slice(&old.lines[old_index..]);
                parsed.checkpoints.extend(
                    old.checkpoints
                        .range(old_index..)
                        .map(|(i, s)| (i - old_index + index, s.clone())),
                );
                break;
            }
            parsed.checkpoints.insert(index, state.clone());
            parser = HighlightLines::from_state(&theme, state.0, state.1);
        }
        let Ok(tokens) = parser.highlight_line(lines[index], &catalog.syntaxes) else {
            return fail("Syntax colors unavailable: grammar could not parse this document");
        };
        #[cfg(test)]
        {
            parsed.parsed_lines += 1;
        }
        let mut offset = 0;
        let mut spans = Vec::new();
        for (style, token) in tokens {
            let end = offset + token.len();
            if Some(style.foreground) != neutral && !token.is_empty() {
                let c = style.foreground;
                push_span(
                    &mut spans,
                    TextStyleSpan {
                        range: offset..end,
                        color: Color::rgba(c.r, c.g, c.b, c.a),
                    },
                );
            }
            offset = end;
        }
        span_count += spans.len();
        if span_count > MAX_SPANS {
            return fail("Syntax colors paused: document has too many tokens");
        }
        parsed.lines.push(spans.into());
    }
    let mut result = Highlight::default();
    let mut offset = 0;
    for (text, spans) in lines.iter().zip(&parsed.lines) {
        for span in spans.iter() {
            push_span(
                &mut result.spans,
                TextStyleSpan {
                    range: (span.range.start + offset)..(span.range.end + offset),
                    color: span.color,
                },
            );
            if result.spans.len() > MAX_SPANS {
                return fail("Syntax colors paused: document has too many tokens");
            }
        }
        offset += text.len();
    }
    (result, Some(parsed))
}

fn push_span(spans: &mut Vec<TextStyleSpan>, span: TextStyleSpan) {
    if let Some(last) = spans
        .last_mut()
        .filter(|last| last.range.end == span.range.start && last.color == span.color)
    {
        last.range.end = span.range.end;
    } else {
        spans.push(span);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::syntax::tests::{catalog, request};

    #[test]
    fn a_local_edit_reparses_a_bounded_region_and_matches_a_full_parse() {
        let source = (0..1000)
            .map(|n| format!("fn item_{n}() {{ let value = {n}; }}\n"))
            .collect::<String>();
        let mut cache = WorkerCache::default();
        let first = request(4, 1, "a.rs", &source);
        let initial = cache.highlight(catalog(), &first);
        assert!(initial.fallback.is_none(), "{:?}", initial.fallback);
        assert_eq!(cache.0.back().unwrap().1.parsed_lines, 1000);
        for (revision, text) in [
            source.replace("value = 500", "value = 123456"),
            source.replace("fn item_500", "// 日本語\nfn item_500"),
            source.clone(),
        ]
        .into_iter()
        .enumerate()
        {
            let request = request(4, revision as u64 + 2, "a.rs", &text);
            let highlight = cache.highlight(catalog(), &request);
            let count = cache.0.back().unwrap().1.parsed_lines;
            assert!(count <= 64, "parsed {count} lines for a local edit");
            assert_eq!(highlight.spans, full(catalog(), &request).spans);
            eprintln!("incremental syntax: {count}/1000 lines");
        }
    }

    #[test]
    fn multiline_states_and_removed_delimiters_propagate_until_they_converge() {
        let body = "let value = 1;\n".repeat(96);
        for (name, initial, changed) in [
            (
                "a.rs",
                format!("/* comment */\n{body}"),
                format!("/* comment\n{body}"),
            ),
            (
                "a.py",
                format!("text = \"ok\"\n{body}"),
                format!("text = \"\"\"open\n{body}"),
            ),
            (
                "a.toml",
                format!("text = \"ok\"\n{body}"),
                format!("text = \"\"\"open\n{body}"),
            ),
            (
                "a.ts",
                format!("const s = `ok`;\n{body}"),
                format!("const s = `open\n{body}"),
            ),
        ] {
            let mut cache = WorkerCache::default();
            for (i, text) in [&initial, &changed, &initial].into_iter().enumerate() {
                let req = request(1, i as u64, name, text);
                let actual = cache.highlight(catalog(), &req);
                assert_eq!(
                    actual.spans,
                    full(catalog(), &req).spans,
                    "{name} revision {i}"
                );
                if i == 1 {
                    assert!(
                        cache.0.back().unwrap().1.parsed_lines > 64,
                        "stopped before multiline state propagated: {name}"
                    );
                }
            }
        }
    }
}
