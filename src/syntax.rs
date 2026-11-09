//! Revision-keyed, asynchronous syntax highlighting. No parsing runs on paint.
//!
//! Call `spans` with the engine revision (also after undo/redo and Save As), and
//! call `poll` from the app idle hook to request a view update after completion.
//! While parsing an edit, colors for unchanged text are remapped to its new
//! byte offsets. One worker owns grammars; requests coalesce by document ID.
mod incremental;
use crate::theme::Theme as EditorTheme;
use scarlet_ui::prelude::{Color, TextStyleSpan};
use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    sync::{
        Arc, Condvar, Mutex, OnceLock,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::{Duration, Instant},
};
use syntect::{
    easy::HighlightLines,
    parsing::{SyntaxDefinition, SyntaxReference, SyntaxSet},
    util::LinesWithEndings,
};

/// Limits protect editor latency and memory. The entire document falls back to
/// plain text rather than starting a parser halfway through a multiline token.
pub const MAX_BYTES: usize = 1024 * 1024;
const MAX_LINES: usize = 20_000;
const MAX_LINE_BYTES: usize = 16 * 1024;
const MAX_SPANS: usize = 80_000;
const MAX_DOCUMENTS: usize = 12;
const MAX_PENDING: usize = 8;
const PARSE_BUDGET: Duration = Duration::from_secs(2);

#[derive(Clone, Debug, PartialEq, Eq)]
struct Key {
    revision: u64,
    path: Option<PathBuf>,
}
impl Key {
    fn matches(&self, revision: u64, path: Option<&Path>) -> bool {
        self.revision == revision && self.path.as_deref() == path
    }
}
#[derive(Default)]
struct Highlight {
    spans: Vec<TextStyleSpan>,
    fallback: Option<&'static str>,
}
impl Highlight {
    fn plain(reason: &'static str) -> Self {
        Self {
            fallback: Some(reason),
            ..Self::default()
        }
    }
}
struct Entry {
    key: Key,
    text: Arc<str>,
    cancel: Arc<AtomicBool>,
    used: u64,
    highlight: Arc<Highlight>,
    generation: u64,
}
struct Request {
    id: u64,
    key: Key,
    cancel: Arc<AtomicBool>,
    text: String,
    theme: EditorTheme,
}
#[derive(Default)]
struct Cache {
    entries: HashMap<u64, Entry>,
    pending: VecDeque<Request>,
    clock: u64,
    changed: bool,
    theme: EditorTheme,
}
impl Cache {
    fn set_theme(&mut self, theme: EditorTheme) -> bool {
        if self.theme == theme {
            return false;
        }
        for entry in self.entries.values() {
            entry.cancel.store(true, Ordering::Relaxed);
        }
        self.entries.clear();
        self.pending.clear();
        self.theme = theme;
        self.changed = true;
        true
    }

    fn remove(&mut self, id: u64) {
        if let Some(entry) = self.entries.remove(&id) {
            entry.cancel.store(true, Ordering::Relaxed);
        }
        self.pending.retain(|request| request.id != id);
    }

    fn request(
        &mut self,
        id: u64,
        revision: u64,
        path: Option<&Path>,
        text: &str,
    ) -> Vec<TextStyleSpan> {
        self.clock = self.clock.wrapping_add(1);
        if let Some(entry) = self.entries.get_mut(&id) {
            if entry.key.matches(revision, path) {
                entry.used = self.clock;
                return entry.highlight.spans.clone();
            }
        }
        let provisional = self
            .entries
            .get(&id)
            .filter(|entry| entry.key.path.as_deref() == path && text.len() <= MAX_BYTES)
            .map(|entry| remap_spans(&entry.text, text, &entry.highlight.spans))
            .unwrap_or_default();
        self.remove(id);
        if self.entries.len() >= MAX_DOCUMENTS {
            if let Some((&oldest, _)) = self.entries.iter().min_by_key(|(_, entry)| entry.used) {
                self.remove(oldest);
            }
        }
        let key = Key {
            revision,
            path: path.map(Path::to_path_buf),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let highlight = if text.len() > MAX_BYTES {
            Highlight::plain("Syntax colors paused: document exceeds 1 MiB")
        } else {
            if self.pending.len() >= MAX_PENDING {
                // Dropping the entry makes that document eligible for retry
                // on its next view update; it cannot be stuck pending forever.
                if let Some(oldest) = self.pending.pop_front() {
                    self.remove(oldest.id);
                }
            }
            self.pending.push_back(Request {
                id,
                key: key.clone(),
                cancel: cancel.clone(),
                text: text.to_owned(),
                theme: self.theme.clone(),
            });
            Highlight {
                spans: provisional,
                fallback: None,
            }
        };
        let spans = highlight.spans.clone();
        self.entries.insert(
            id,
            Entry {
                key,
                text: if text.len() <= MAX_BYTES {
                    Arc::from(text)
                } else {
                    Arc::from("")
                },
                cancel,
                used: self.clock,
                highlight: Arc::new(highlight),
                generation: self.clock,
            },
        );
        spans
    }

    fn publish(&mut self, request: &Request, highlight: Highlight) -> bool {
        let Some(entry) = self.entries.get_mut(&request.id) else {
            return false;
        };
        // Token identity also protects a -> b -> a transitions and eviction /
        // re-request of the same revision. Key equality alone is insufficient.
        if entry.key != request.key
            || !Arc::ptr_eq(&entry.cancel, &request.cancel)
            || request.cancel.load(Ordering::Relaxed)
        {
            return false;
        }
        self.clock = self.clock.wrapping_add(1);
        entry.highlight = Arc::new(highlight);
        entry.generation = self.clock;
        self.changed = true;
        true
    }
}
/// Keep only unchanged pieces of the previous highlight, translated through a
/// UTF-8-safe replacement range. Never expose old offsets against new text.
fn remap_spans(before: &str, after: &str, spans: &[TextStyleSpan]) -> Vec<TextStyleSpan> {
    let mut start = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !before.is_char_boundary(start) || !after.is_char_boundary(start) {
        start -= 1;
    }
    let mut suffix = before.as_bytes()[start..]
        .iter()
        .rev()
        .zip(after.as_bytes()[start..].iter().rev())
        .take_while(|(a, b)| a == b)
        .count();
    while !before.is_char_boundary(before.len() - suffix)
        || !after.is_char_boundary(after.len() - suffix)
    {
        suffix -= 1;
    }
    let old_end = before.len() - suffix;
    let new_end = after.len() - suffix;
    let mut result = Vec::with_capacity(spans.len() + 1);
    for span in spans {
        let left = span.range.start..span.range.end.min(start);
        if left.start < left.end {
            result.push(TextStyleSpan {
                range: left,
                color: span.color,
            });
        }
        let right = span.range.start.max(old_end)..span.range.end;
        if right.start < right.end {
            result.push(TextStyleSpan {
                range: (right.start - old_end + new_end)..(right.end - old_end + new_end),
                color: span.color,
            });
        }
    }
    result
}

struct Service {
    shared: Arc<(Mutex<Cache>, Condvar)>,
    running: bool,
}
impl Service {
    fn new() -> Self {
        Self::with_theme(EditorTheme::default())
    }
    fn with_theme(theme: EditorTheme) -> Self {
        let shared = Arc::new((
            Mutex::new(Cache {
                theme,
                ..Cache::default()
            }),
            Condvar::new(),
        ));
        let worker = shared.clone();
        let running = std::thread::Builder::new()
            .name("kureha-syntax".into())
            .stack_size(4 * 1024 * 1024)
            .spawn(move || {
                let mut incremental = incremental::WorkerCache::default();
                loop {
                    let request = {
                        let (lock, wake) = &*worker;
                        let mut cache = lock.lock().unwrap();
                        while cache.pending.is_empty() {
                            cache = wake.wait(cache).unwrap();
                        }
                        cache.pending.pop_front().unwrap()
                    };
                    if request.cancel.load(Ordering::Relaxed) {
                        continue;
                    }
                    // Grammar decompression and regex compilation happen here,
                    // including on first use, outside the UI/cache lock.
                    let catalog = CATALOG.get_or_init(Catalog::new);
                    let highlight = incremental.highlight(catalog, &request);
                    worker.0.lock().unwrap().publish(&request, highlight);
                }
            })
            .is_ok();
        Self { shared, running }
    }
}
static SERVICE: OnceLock<Service> = OnceLock::new();
static CATALOG: OnceLock<Catalog> = OnceLock::new();

struct SnippetSource {
    text: String,
    path: Option<PathBuf>,
    id: u64,
}
struct Snippets {
    service: Service,
    sources: Mutex<VecDeque<SnippetSource>>,
    next_id: AtomicU64,
}
static SNIPPETS: [OnceLock<Snippets>; 3] = [const { OnceLock::new() }; 3];

/// Bounded, asynchronous hover highlighting with the editor's grammars/colors.
/// Each theme has a small independent cache so hovers cannot evict documents
/// or briefly reuse colors from the other theme. `poll` reports their results.
pub fn snippet_spans(path: Option<&Path>, text: &str, theme: EditorTheme) -> Vec<TextStyleSpan> {
    if text.len() > 20_000 || text.is_empty() {
        return Vec::new();
    }
    let snippets = SNIPPETS[match &theme {
        EditorTheme::Light => 0,
        EditorTheme::Dark => 1,
        EditorTheme::Custom(_) => 2,
    }]
    .get_or_init(|| Snippets {
        service: Service::with_theme(theme.clone()),
        sources: Mutex::new(VecDeque::new()),
        next_id: AtomicU64::new(1),
    });
    if !snippets.service.running {
        return Vec::new();
    }
    let id = {
        let mut sources = snippets.sources.lock().unwrap();
        let source = if let Some(index) = sources
            .iter()
            .position(|source| source.text == text && source.path.as_deref() == path)
        {
            sources.remove(index).unwrap()
        } else {
            if sources.len() >= 8 {
                sources.pop_front();
            }
            SnippetSource {
                text: text.into(),
                path: path.map(Path::to_path_buf),
                id: snippets.next_id.fetch_add(1, Ordering::Relaxed),
            }
        };
        let id = source.id;
        sources.push_back(source);
        id
    };
    let spans = {
        let mut cache = snippets.service.shared.0.lock().unwrap();
        cache.set_theme(theme);
        cache.request(id, 0, path, text)
    };
    snippets.service.shared.1.notify_one();
    spans
}

/// Foreground spans with offsets valid for this document revision and path.
/// Repeated paints do not copy text, hash text, enqueue jobs, or reparse it.
/// Edits retain translated colors on unchanged text until parsing completes.
/// First loads and explicit fallbacks return an empty vector (plain text).
pub fn spans(id: u64, revision: u64, path: Option<&Path>, text: &str) -> Vec<TextStyleSpan> {
    let service = SERVICE.get_or_init(Service::new);
    if !service.running {
        return Vec::new();
    }
    let result = service
        .shared
        .0
        .lock()
        .unwrap()
        .request(id, revision, path, text);
    service.shared.1.notify_one();
    result
}

/// True once for newly published, non-stale work; refresh decoration consumers.
/// Does not start a worker or load grammars until content requests spans.
pub fn poll() -> bool {
    let mut changed = SERVICE
        .get()
        .is_some_and(|service| std::mem::take(&mut service.shared.0.lock().unwrap().changed));
    for snippets in &SNIPPETS {
        if let Some(snippets) = snippets.get() {
            changed |= std::mem::take(&mut snippets.service.shared.0.lock().unwrap().changed);
        }
    }
    changed
}

/// Select the shared syntax theme. A change cancels queued/in-flight work and
/// drops all old colors under the cache lock. Old requests cannot publish even
/// after Light -> Dark -> Light; identity tokens protect that round trip.
/// Returns whether the theme changed; the existing `poll` also reports it.
/// The next `spans` call for each document requests its new colors.
pub fn set_theme(theme: EditorTheme) -> bool {
    let service = SERVICE.get_or_init(Service::new);
    service.shared.0.lock().unwrap().set_theme(theme)
}

pub fn theme() -> EditorTheme {
    SERVICE.get().map_or(EditorTheme::default(), |service| {
        service.shared.0.lock().unwrap().theme.clone()
    })
}

/// Shared, immutable colors for a minimap or another decoration consumer.
/// An empty snapshot can mean parsing is pending or a documented fallback.
#[derive(Clone)]
pub struct Snapshot {
    highlight: Arc<Highlight>,
    pub generation: u64,
    pub theme: EditorTheme,
}
impl Snapshot {
    pub fn spans(&self) -> &[TextStyleSpan] {
        &self.highlight.spans
    }
}

/// Read without copying spans or source, starting work, or updating LRU usage.
/// Request `spans` first and refresh after `poll` reports completion.
pub fn cached(id: u64, revision: u64, path: Option<&Path>) -> Option<Snapshot> {
    let cache = SERVICE.get()?.shared.0.lock().unwrap();
    let entry = cache.entries.get(&id)?;
    if !entry.key.matches(revision, path) {
        return None;
    }
    Some(Snapshot {
        highlight: entry.highlight.clone(),
        generation: entry.generation,
        theme: cache.theme.clone(),
    })
}

/// User-visible explanation for an explicit plain-text fallback, if any.
pub fn fallback_reason(id: u64, revision: u64, path: Option<&Path>) -> Option<&'static str> {
    let service = SERVICE.get()?;
    if !service.running {
        return Some("Syntax colors unavailable: could not start worker");
    }
    let cache = service.shared.0.lock().unwrap();
    cache
        .entries
        .get(&id)
        .filter(|entry| entry.key.matches(revision, path))
        .and_then(|entry| entry.highlight.fallback)
}

struct Catalog {
    syntaxes: SyntaxSet,
}
impl Catalog {
    fn new() -> Self {
        let mut builder = SyntaxSet::load_defaults_newlines().into_builder();
        for source in [TYPESCRIPT, TOML] {
            builder.add(
                SyntaxDefinition::load_from_str(source, true, None)
                    .expect("built-in Kureha grammar must be valid"),
            );
        }
        Self {
            syntaxes: builder.build(),
        }
    }

    fn syntax(&self, path: Option<&Path>, text: &str) -> &SyntaxReference {
        // Only inspect the supplied snapshot. find_syntax_for_file can read the
        // disk, which may differ from the dirty buffer or be a network path.
        let syntax = path.and_then(|path| {
            let name = path.file_name()?.to_str()?;
            self.syntaxes.find_syntax_by_extension(name).or_else(|| {
                let extension = path.extension()?.to_str()?.to_ascii_lowercase();
                self.syntaxes.find_syntax_by_extension(&extension)
            })
        });
        syntax
            .or_else(|| {
                self.syntaxes
                    .find_syntax_by_first_line(text.lines().next().unwrap_or(""))
            })
            .unwrap_or_else(|| self.syntaxes.find_syntax_plain_text())
    }

    #[cfg(test)]
    fn highlight(&self, request: &Request) -> Highlight {
        incremental::full(self, request)
    }
}

// Local Sublime grammars fill gaps in syntect's bundled set. Context stacks
// retain comments/strings across physical lines; template interpolation nests
// braces without terminating a surrounding string. This is lexical coloring,
// independent of language-server semantic tokens.
const TYPESCRIPT: &str = r#"%YAML 1.2
---
name: TypeScript
file_extensions: [ts, tsx, mts, cts]
scope: source.ts
contexts:
  main:
    - include: comments
    - include: strings
    - match: '\b(?:abstract|any|as|asserts|async|await|bigint|boolean|break|case|catch|class|const|constructor|continue|debugger|declare|default|delete|do|else|enum|export|extends|finally|for|from|function|get|global|if|implements|import|in|infer|instanceof|interface|is|keyof|let|module|namespace|never|new|number|object|of|override|private|protected|public|readonly|require|return|satisfies|set|static|string|super|switch|symbol|this|throw|try|type|typeof|undefined|unique|unknown|using|var|void|while|with|yield)\b'
      scope: keyword.control.ts
    - match: '\b(?:true|false|null|NaN|Infinity)\b'
      scope: constant.language.ts
    - match: '\b(?:0[xX][\da-fA-F_]+|0[bB][01_]+|0[oO][0-7_]+|\d[\d_]*(?:\.[\d_]*)?(?:[eE][+-]?[\d_]+)?n?)\b'
      scope: constant.numeric.ts
    - match: '\b[A-Z][\w$]*\b'
      scope: entity.name.type.ts
    - match: '[\w$]+(?=\s*\()'
      scope: entity.name.function.ts
    - match: '=>|[=+*%!?&|^~<>:-]+'
      scope: keyword.operator.ts
    - match: '(?<=[=(:,\[!&|?;{}])\s*(/)(?![/*])|^\s*(/)(?![/*])'
      captures:
        1: punctuation.definition.string.begin.ts
        2: punctuation.definition.string.begin.ts
      push: regex
    - match: '\{'
      push: braces
  braces:
    - match: '\}'
      pop: true
    - include: main
  comments:
    - match: '//'
      scope: punctuation.definition.comment.ts
      push:
        - meta_scope: comment.line.double-slash.ts
        - match: '$'
          pop: true
    - match: '/\*'
      scope: punctuation.definition.comment.ts
      push:
        - meta_scope: comment.block.ts
        - match: '\*/'
          pop: true
  strings:
    - match: '"'
      push: double
    - match: "'"
      push: single
    - match: '`'
      push: template
  double:
    - meta_scope: string.quoted.double.ts
    - match: '\\[\s\S]'
      scope: constant.character.escape.ts
    - match: '"|(?=\n)'
      pop: true
  single:
    - meta_scope: string.quoted.single.ts
    - match: '\\[\s\S]'
      scope: constant.character.escape.ts
    - match: "'|(?=\n)"
      pop: true
  template:
    - meta_scope: string.quoted.other.ts
    - match: '\\[\s\S]'
      scope: constant.character.escape.ts
    - match: '`'
      pop: true
    - match: '\$\{'
      push:
        - clear_scopes: true
        - meta_scope: source.ts
        - match: '\}'
          pop: true
        - include: main
  regex:
    - meta_scope: string.regexp.ts
    - match: '\\[\s\S]'
      scope: constant.character.escape.ts
    - match: '\['
      push:
        - match: '\\[\s\S]'
        - match: '\]'
          pop: true
    - match: '/[dgimsuvy]*|(?=\n)'
      pop: true
"#;

const TOML: &str = r#"%YAML 1.2
---
name: TOML
file_extensions: [toml, Cargo.lock]
scope: source.toml
contexts:
  main:
    - match: '#'
      push:
        - meta_scope: comment.line.number-sign.toml
        - match: '$'
          pop: true
    - match: '"""'
      push: multiline-basic
    - match: "'''"
      push: multiline-literal
    - match: '"'
      push: basic
    - match: "'"
      push: literal
    - match: '^\s*(\[\[?)([^\]\n]+)(\]\]?)'
      captures:
        1: punctuation.definition.table.toml
        2: entity.name.section.toml
        3: punctuation.definition.table.toml
    - match: '[\w-]+(?=\s*[.=])'
      scope: entity.name.tag.toml
    - match: '\b(?:true|false)\b'
      scope: constant.language.boolean.toml
    - match: '[+-]?(?:inf|nan)\b|[+-]?\b\d[\w.:+-]*\b'
      scope: constant.numeric.toml
    - match: '='
      scope: keyword.operator.assignment.toml
  basic:
    - meta_scope: string.quoted.double.toml
    - match: '\\.'
      scope: constant.character.escape.toml
    - match: '"|(?=\n)'
      pop: true
  literal:
    - meta_scope: string.quoted.single.toml
    - match: "'|(?=\n)"
      pop: true
  multiline-basic:
    - meta_scope: string.quoted.triple.double.toml
    - match: '\\[\s\S]'
      scope: constant.character.escape.toml
    - match: '"{3,5}'
      pop: true
  multiline-literal:
    - meta_scope: string.quoted.triple.single.toml
    - match: "'{3,5}"
      pop: true
"#;

#[cfg(test)]
mod tests {
    use super::*;

    pub(super) fn catalog() -> &'static Catalog {
        static CATALOG: OnceLock<Catalog> = OnceLock::new();
        CATALOG.get_or_init(Catalog::new)
    }
    pub(super) fn request(id: u64, revision: u64, name: &str, text: &str) -> Request {
        Request {
            id,
            key: Key {
                revision,
                path: Some(PathBuf::from(name)),
            },
            cancel: Arc::new(AtomicBool::new(false)),
            text: text.into(),
            theme: EditorTheme::Light,
        }
    }
    fn highlight(name: &str, text: &str) -> Highlight {
        let result = catalog().highlight(&request(1, 0, name, text));
        assert!(result.fallback.is_none(), "{}: {:?}", name, result.fallback);
        for span in &result.spans {
            assert!(span.range.start < span.range.end);
            assert!(text.is_char_boundary(span.range.start));
            assert!(text.is_char_boundary(span.range.end));
            assert!(span.range.end <= text.len());
        }
        result
    }
    fn color_at(result: &Highlight, byte: usize) -> Option<Color> {
        result
            .spans
            .iter()
            .find(|span| span.range.contains(&byte))
            .map(|span| span.color)
    }
    fn color_of(result: &Highlight, text: &str, token: &str) -> Option<Color> {
        color_at(result, text.find(token).expect("test token"))
    }
    fn assert_same_color(result: &Highlight, text: &str, tokens: &[&str]) {
        let expected = color_of(result, text, tokens[0]);
        assert!(expected.is_some(), "uncolored token: {}", tokens[0]);
        for token in &tokens[1..] {
            assert_eq!(
                expected,
                color_of(result, text, token),
                "wrong lexical context for {token}"
            );
        }
    }

    #[test]
    fn rust_nested_multiline_comments_and_raw_strings_keep_utf8_offsets() {
        let text = "/* comment start\n /* nested */ still comment\n end */\nfn main() { let 名前 = r###\"raw start\n// string body /* not comment */\nraw end\"###; }\n";
        let result = highlight("main.rs", text);
        assert_same_color(&result, text, &["comment start", "still comment", "end */"]);
        assert_same_color(&result, text, &["raw start", "string body", "raw end"]);
        assert_ne!(
            color_of(&result, text, "comment start"),
            color_of(&result, text, "raw start")
        );
        assert_ne!(
            color_of(&result, text, "fn main"),
            color_of(&result, text, "raw start")
        );
    }
    #[test]
    fn python_triple_strings_do_not_turn_hashes_into_comments() {
        let text = "value = '''triple start\n# string body with 猫\ntriple end'''\n# real comment\ndef greet():\n    return \"hello\"\n";
        let result = highlight("script.py", text);
        assert_same_color(
            &result,
            text,
            &["triple start", "string body", "triple end"],
        );
        assert_ne!(
            color_of(&result, text, "real comment"),
            color_of(&result, text, "triple start")
        );
        assert_ne!(
            color_of(&result, text, "def greet"),
            color_of(&result, text, "triple start")
        );
    }
    #[test]
    fn javascript_comments_and_template_strings_cross_lines() {
        let text = "/* block start\n block end */\nconst value = `template start\n// string body\ntemplate end`;\nfunction greet() { return true; }\n";
        let result = highlight("script.js", text);
        assert_same_color(&result, text, &["block start", "block end"]);
        assert_same_color(
            &result,
            text,
            &["template start", "string body", "template end"],
        );
        assert_ne!(
            color_of(&result, text, "block start"),
            color_of(&result, text, "template start")
        );
    }
    #[test]
    fn typescript_handles_types_multiline_comments_and_nested_templates() {
        let text = "/* block start\n block end */\ninterface Person { name: string; }\nconst x: Person = {name: `template start\n// string body ${ {nested: `inner ${true}`}.nested } template end`};\n";
        let result = highlight("script.ts", text);
        assert_same_color(&result, text, &["block start", "block end"]);
        assert_same_color(
            &result,
            text,
            &["template start", "string body", "template end"],
        );
        assert_ne!(
            color_of(&result, text, "interface"),
            color_of(&result, text, "template start")
        );
        assert_ne!(
            color_of(&result, text, "true"),
            color_of(&result, text, "template start")
        );
    }
    #[test]
    fn toml_multiline_basic_and_literal_strings_contain_hashes() {
        let text = "[package]\nname = \"kureha\"\nfirst = \"\"\"basic start\n# basic body\nbasic end\"\"\"\nsecond = '''literal start\n# literal body\nliteral end'''\n# actual comment\nenabled = true\n";
        let result = highlight("Cargo.toml", text);
        assert_same_color(&result, text, &["basic start", "basic body", "basic end"]);
        assert_same_color(
            &result,
            text,
            &["literal start", "literal body", "literal end"],
        );
        assert_ne!(
            color_of(&result, text, "actual comment"),
            color_of(&result, text, "basic start")
        );
        assert!(color_of(&result, text, "true").is_some());
    }
    #[test]
    fn json_escapes_markdown_fences_and_common_formats_are_colored() {
        let text = "{\"名前\": \"quote \\\" // string body\", \"n\": 42, \"enabled\": true}";
        let result = highlight("data.json", text);
        assert!(color_of(&result, text, "string body").is_some());
        assert!(color_of(&result, text, "42").is_some());
        assert!(color_of(&result, text, "true").is_some());
        let markdown = "# Header\n\n**bold** and `inline`\n\n```rust\nfn main() {}\n```\n";
        assert!(!highlight("README.md", markdown).spans.is_empty());
        for (name, text) in [
            ("style.css", "body { color: red; }"),
            ("index.html", "<div class=\"hello\">text</div>"),
            ("config.yaml", "enabled: true\nname: 'hello'\n"),
            ("script.sh", "#!/bin/sh\necho \"hello\"\n"),
        ] {
            assert!(!highlight(name, text).spans.is_empty(), "{name}");
        }
        assert!(highlight("notes.txt", "a plain note").spans.is_empty());
    }
    #[test]
    fn syntax_detection_uses_snapshot_shebang_case_and_special_names() {
        for (path, first, expected) in [
            (Some("main.RS"), "", "Rust"),
            (Some("app.tsx"), "", "TypeScript"),
            (Some("Cargo.lock"), "", "TOML"),
            (None, "#!/usr/bin/env python3\n", "Python"),
            (Some("Makefile"), "", "Makefile"),
        ] {
            assert_eq!(catalog().syntax(path.map(Path::new), first).name, expected);
        }
    }
    #[test]
    fn cache_hits_do_not_reenqueue_and_revisions_paths_and_ids_invalidate() {
        let mut cache = Cache::default();
        cache.request(11, 1, Some(Path::new("a.rs")), "fn old() {}");
        let first = cache.pending.pop_front().unwrap();
        let colored = TextStyleSpan {
            range: 0..2,
            color: Color::rgb(12, 34, 56),
        };
        assert!(cache.publish(
            &first,
            Highlight {
                spans: vec![colored.clone()],
                fallback: None
            }
        ));
        assert_eq!(
            cache.request(11, 1, Some(Path::new("a.rs")), "fn old() {}"),
            vec![colored.clone()]
        );
        assert!(cache.pending.is_empty());
        assert!(
            cache
                .request(12, 1, Some(Path::new("a.rs")), "fn old() {}")
                .is_empty()
        );
        assert_eq!(
            cache.request(11, 2, Some(Path::new("a.rs")), "fn new() {}"),
            vec![colored]
        );
        assert!(first.cancel.load(Ordering::Relaxed));
        assert!(!cache.publish(&first, Highlight::default()));
        cache.request(11, 2, Some(Path::new("a.py")), "fn new() {}");
        assert_eq!(cache.pending.iter().filter(|r| r.id == 11).count(), 1);
        assert_eq!(
            cache.entries[&11].key.path.as_deref(),
            Some(Path::new("a.py"))
        );
    }
    #[test]
    fn provisional_colors_follow_unicode_insert_delete_and_undo_without_blank_frames() {
        let mut cache = Cache::default();
        let path = Some(Path::new("unicode.rs"));
        let original = "// 日本語\nfn main() { let n = 1; }\n";
        cache.request(7, 1, path, original);
        let first = cache.pending.pop_front().unwrap();
        assert!(cache.publish(&first, catalog().highlight(&first)));
        let color = cache.entries[&7]
            .highlight
            .spans
            .iter()
            .find(|s| s.range.contains(&original.find("fn").unwrap()))
            .unwrap()
            .color;
        for (revision, text) in [
            "// 日本語\n// 追加\nfn main() { let n = 1; }\n",
            "// 語\nfn main() { let n = 1; }\n",
            original,
        ]
        .into_iter()
        .enumerate()
        {
            let spans = cache.request(7, revision as u64 + 2, path, text);
            assert!(!spans.is_empty());
            let keyword = text.find("fn").unwrap();
            assert_eq!(
                spans
                    .iter()
                    .find(|s| s.range.contains(&keyword))
                    .unwrap()
                    .color,
                color
            );
            for span in &spans {
                assert!(span.range.start < span.range.end && span.range.end <= text.len());
                assert!(
                    text.is_char_boundary(span.range.start)
                        && text.is_char_boundary(span.range.end)
                );
            }
        }
        assert!(
            !cache.publish(&first, Highlight::default()),
            "stale parse overwrote remapped spans"
        );
        assert!(
            cache
                .request(7, 5, Some(Path::new("unicode.txt")), original)
                .is_empty()
        );
        cache.set_theme(EditorTheme::Dark);
        assert!(cache.request(7, 5, path, original).is_empty());
    }

    #[test]
    fn stale_completion_cannot_overwrite_same_key_after_eviction_or_path_roundtrip() {
        let mut cache = Cache::default();
        cache.request(1, 1, None, "original");
        let original = cache.pending.pop_front().unwrap();
        cache.request(1, 1, Some(Path::new("a.rs")), "original");
        cache.request(1, 1, None, "original");
        assert!(!cache.publish(&original, Highlight::default()));
        let second = cache.pending.pop_front().unwrap();
        cache.remove(1);
        cache.request(1, 1, None, "original");
        assert!(!cache.publish(&second, Highlight::default()));
    }
    #[test]
    fn queue_coalesces_and_memory_is_bounded() {
        let mut cache = Cache::default();
        for revision in 0..1_000 {
            cache.request(1, revision, None, "small");
            assert_eq!(cache.pending.len(), 1);
        }
        assert_eq!(cache.pending.front().unwrap().key.revision, 999);
        for id in 2..100 {
            cache.request(id, 1, None, "small");
            assert!(cache.entries.len() <= MAX_DOCUMENTS);
            assert!(cache.pending.len() <= MAX_PENDING);
        }
        // An evicted pending document can be requested again normally.
        cache.request(1, 999, None, "small");
        assert_eq!(cache.pending.back().unwrap().id, 1);
    }
    #[test]
    fn large_or_pathological_documents_have_explicit_whole_document_fallbacks() {
        let mut cache = Cache::default();
        cache.request(1, 1, Some(Path::new("big.rs")), &"a".repeat(MAX_BYTES + 1));
        assert!(cache.pending.is_empty());
        assert!(
            cache.entries[&1]
                .highlight
                .fallback
                .unwrap()
                .contains("1 MiB")
        );
        for text in [
            "x".repeat(MAX_LINE_BYTES + 1),
            "// x\n".repeat(MAX_LINES + 1),
        ] {
            let result = catalog().highlight(&request(1, 1, "big.rs", &text));
            assert!(result.spans.is_empty());
            assert!(result.fallback.is_some());
        }
    }
    #[test]
    fn custom_palette_reload_rejects_old_jobs_and_uses_new_token_colors() {
        let mut cache = Cache::default();
        cache.set_theme(crate::theme::parse_theme("[syntax]\nkeyword='#112233'").unwrap());
        cache.request(1, 0, Some(Path::new("main.rs")), "fn main() {}");
        let old = cache.pending.pop_front().unwrap();
        cache.set_theme(crate::theme::parse_theme("[syntax]\nkeyword='#aabbcc'").unwrap());
        assert!(!cache.publish(&old, catalog().highlight(&old)));
        cache.request(1, 0, Some(Path::new("main.rs")), "fn main() {}");
        let fresh = cache.pending.pop_front().unwrap();
        let colors = catalog().highlight(&fresh);
        assert_eq!(color_at(&colors, 0), Some(Color::rgb(170, 187, 204)));
        assert!(cache.publish(&fresh, colors));
    }

    #[test]
    fn theme_switch_cancels_pending_and_inflight_and_rejects_roundtrip_results() {
        let mut cache = Cache::default();
        cache.request(1, 1, Some(Path::new("main.rs")), "fn main() {}");
        let in_flight = cache.pending.pop_front().unwrap();
        cache.request(2, 1, None, "pending");
        let pending = cache.pending.front().unwrap().cancel.clone();
        assert!(cache.set_theme(EditorTheme::Dark));
        assert!(cache.entries.is_empty() && cache.pending.is_empty());
        assert!(cache.changed);
        assert!(pending.load(Ordering::Relaxed));
        assert!(in_flight.cancel.load(Ordering::Relaxed));
        assert!(!cache.publish(&in_flight, Highlight::default()));
        cache.changed = false;
        assert!(!cache.set_theme(EditorTheme::Dark));
        assert!(!cache.changed);
        assert!(cache.set_theme(EditorTheme::Light));
        cache.request(1, 1, Some(Path::new("main.rs")), "fn main() {}");
        assert!(!cache.publish(&in_flight, Highlight::default()));
        let current = cache.pending.pop_front().unwrap();
        assert_eq!(current.theme, EditorTheme::Light);
        assert!(cache.publish(&current, Highlight::default()));
    }

    #[test]
    fn actual_syntax_colors_change_in_both_themes_without_changing_offsets() {
        let text = "// comment\nfn greet() { let value = \"hello 猫\"; let n = 42; }\n";
        let light = catalog().highlight(&request(1, 1, "main.rs", text));
        let mut dark_request = request(1, 1, "main.rs", text);
        dark_request.theme = EditorTheme::Dark;
        let dark = catalog().highlight(&dark_request);
        assert!(dark.fallback.is_none());
        for token in ["comment", "fn greet", "hello", "42"] {
            assert!(color_of(&dark, text, token).is_some());
            assert_ne!(color_of(&light, text, token), color_of(&dark, text, token));
        }
        assert_eq!(
            light.spans.iter().map(|s| &s.range).collect::<Vec<_>>(),
            dark.spans.iter().map(|s| &s.range).collect::<Vec<_>>()
        );
    }

    #[test]
    fn asynchronous_completion_wakes_poll_and_drops_replaced_token_colors() {
        let id = u64::MAX - 100;
        let path = Some(Path::new("async.rs"));
        assert!(spans(id, 1, path, "fn main() {}").is_empty());
        let deadline = Instant::now() + Duration::from_secs(15);
        let mut woke = false;
        loop {
            woke |= poll();
            if !spans(id, 1, path, "fn main() {}").is_empty() {
                break;
            }
            assert!(Instant::now() < deadline, "syntax worker did not finish");
            std::thread::sleep(Duration::from_millis(5));
        }
        woke |= poll();
        assert!(woke);
        assert!(!poll());
        assert!(spans(id, 2, path, "// now a comment").is_empty());
    }
}
