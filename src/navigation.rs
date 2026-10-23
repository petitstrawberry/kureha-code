//! Bounded project navigation, independent of the UI and its worker scheduler.
//!
//! Run `scan` and (for large indexes) `quick_open` on a worker. No function here
//! starts threads or changes files. Scans never intentionally follow symlink
//! entries; as with std::fs traversal this is not a sandbox against concurrent
//! replacement of directories. Root `.gitignore`/`.ignore` support is explicitly
//! an exclusion-only subset: `*`, `?`, root-relative paths and trailing `/`.
//! Nested ignore files, negation, escapes, `**` and character classes are not
//! implemented. Unsupported root patterns are reported, never silently parsed.
use std::{
    cmp::Ordering as CmpOrdering,
    collections::BinaryHeap,
    fs,
    io::Read,
    ops::Range,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

pub const MAX_RESULTS: usize = 200;
pub const MAX_QUERY_CHARS: usize = 128;
const MAX_ISSUES: usize = 32;
const MAX_IGNORE_RULES: usize = 256;
const MAX_IGNORE_BYTES: usize = 64 * 1024;
const MAX_PATTERN_BYTES: usize = 512;
const EXCLUDED_DIRECTORIES: &[&str] = &[
    ".git",
    ".hg",
    ".svn",
    "target",
    "node_modules",
    "dist",
    "build",
    ".next",
    ".nuxt",
    ".venv",
    "venv",
    "__pycache__",
];

#[derive(Clone, Debug)]
pub struct ScanOptions {
    pub max_files: usize,
    pub max_entries: usize,
    pub max_directories: usize,
    pub max_depth: usize,
    pub max_path_bytes: usize,
    pub max_total_path_bytes: usize,
    pub max_duration: Duration,
    pub include_hidden_directories: bool,
    pub read_root_ignore_files: bool,
    /// The same exclusion-only subset as root ignore files. No shell expansion.
    pub ignore_patterns: Vec<String>,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_files: 50_000,
            max_entries: 250_000,
            max_directories: 20_000,
            max_depth: 64,
            max_path_bytes: 4096,
            max_total_path_bytes: 32 * 1024 * 1024,
            max_duration: Duration::from_secs(10),
            include_hidden_directories: false,
            read_root_ignore_files: true,
            ignore_patterns: Vec::new(),
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanLimit {
    Files,
    Entries,
    Directories,
    Depth,
    PathBytes,
    TotalPathBytes,
    Time,
    IgnoreRules,
    IgnoreFileBytes,
}
#[derive(Clone, Debug)]
pub struct ScanIssue {
    pub path: PathBuf,
    pub message: String,
}
#[derive(Clone, Debug, Default)]
pub struct ScanReport {
    pub cancelled: bool,
    pub limits: Vec<ScanLimit>,
    pub errors: Vec<ScanIssue>,
    pub suppressed_errors: usize,
    pub entries_visited: usize,
    pub directories_visited: usize,
    pub symlinks_skipped: usize,
    pub non_utf8_skipped: usize,
    pub unsupported_ignore_patterns: usize,
}
impl ScanReport {
    fn limit(&mut self, limit: ScanLimit) {
        if !self.limits.contains(&limit) {
            self.limits.push(limit);
        }
    }
    fn error(&mut self, path: &Path, message: impl Into<String>) {
        if self.errors.len() < MAX_ISSUES {
            self.errors.push(ScanIssue {
                path: path.into(),
                message: message.into(),
            });
        } else {
            self.suppressed_errors += 1;
        }
    }
}
#[derive(Clone, Debug, Default)]
pub struct Index {
    /// Absolute root. Paths in `files` can be passed directly to Open File.
    pub root: PathBuf,
    /// Sorted absolute regular-file paths. Hidden files are included.
    pub files: Vec<PathBuf>,
    /// A limit, cancellation, unsupported filename or I/O error omitted data.
    pub truncated: bool,
    /// Observed entries excluded by policy or a per-entry limit. Unvisited
    /// entries after cancellation/global truncation cannot be counted.
    pub skipped: usize,
    pub report: ScanReport,
}

pub fn scan(root: &Path, cancel: &AtomicBool) -> Result<Index, String> {
    scan_with_options(root, &ScanOptions::default(), cancel)
}

pub fn scan_with_options(
    root: &Path,
    options: &ScanOptions,
    cancel: &AtomicBool,
) -> Result<Index, String> {
    let started = Instant::now();
    let metadata = fs::symlink_metadata(root).map_err(|e| format!("{}: {e}", root.display()))?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(format!(
            "{} is not a regular directory (symlink roots are excluded)",
            root.display()
        ));
    }
    let root = fs::canonicalize(root).map_err(|e| format!("{}: {e}", root.display()))?;
    let mut index = Index {
        root: root.clone(),
        ..Index::default()
    };
    if cancel.load(Ordering::Relaxed) {
        index.report.cancelled = true;
        index.truncated = true;
        return Ok(index);
    }
    // Verify the root is readable even when the configured budget is zero.
    let root_entries = fs::read_dir(&root).map_err(|e| format!("{}: {e}", root.display()))?;
    drop(root_entries);
    let rules = ignore_rules(&root, options, &mut index.report);
    let mut directories = vec![(root, 0usize)];
    let mut scheduled_directories = 1usize;
    let mut path_bytes = 0usize;
    'walk: while let Some((directory, depth)) = directories.pop() {
        if stopped(&mut index.report, cancel, started, options.max_duration) {
            break;
        }
        if index.report.directories_visited >= options.max_directories {
            index.report.limit(ScanLimit::Directories);
            break;
        }
        // Recheck queued entries. A symlink introduced after enumeration should
        // not knowingly be followed by the next read_dir call.
        match fs::symlink_metadata(&directory) {
            Ok(meta) if meta.file_type().is_symlink() => {
                index.report.symlinks_skipped += 1;
                index.skipped += 1;
                continue;
            }
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => {
                index
                    .report
                    .error(&directory, "Directory changed during indexing");
                continue;
            }
            Err(error) => {
                index.report.error(&directory, error.to_string());
                continue;
            }
        }
        let entries = match fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) => {
                index.report.error(&directory, error.to_string());
                continue;
            }
        };
        index.report.directories_visited += 1;
        for entry in entries {
            if stopped(&mut index.report, cancel, started, options.max_duration) {
                break 'walk;
            }
            if index.report.entries_visited >= options.max_entries {
                index.report.limit(ScanLimit::Entries);
                break 'walk;
            }
            index.report.entries_visited += 1;
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    index.report.error(&directory, error.to_string());
                    continue;
                }
            };
            let path = entry.path();
            if path.as_os_str().len() > options.max_path_bytes {
                index.skipped += 1;
                index.report.limit(ScanLimit::PathBytes);
                continue;
            }
            let kind = match entry.file_type() {
                Ok(kind) => kind,
                Err(error) => {
                    index.report.error(&path, error.to_string());
                    continue;
                }
            };
            if kind.is_symlink() {
                index.skipped += 1;
                index.report.symlinks_skipped += 1;
                continue;
            }
            let relative = path.strip_prefix(&index.root).unwrap_or(&path);
            let Some(relative) = relative.to_str() else {
                index.skipped += 1;
                index.report.non_utf8_skipped += 1;
                continue;
            };
            let relative = relative.replace(std::path::MAIN_SEPARATOR, "/");
            let name = entry.file_name();
            let name = name.to_str().unwrap_or("");
            if (kind.is_dir()
                && (EXCLUDED_DIRECTORIES.contains(&name)
                    || (!options.include_hidden_directories && name.starts_with('.'))))
                || rules
                    .iter()
                    .any(|rule| rule.matches(&relative, kind.is_dir()))
            {
                index.skipped += 1;
                continue;
            }
            if kind.is_dir() {
                if depth >= options.max_depth {
                    index.skipped += 1;
                    index.report.limit(ScanLimit::Depth);
                } else if scheduled_directories >= options.max_directories {
                    index.skipped += 1;
                    index.report.limit(ScanLimit::Directories);
                } else {
                    scheduled_directories += 1;
                    directories.push((path, depth + 1));
                }
            } else if kind.is_file() {
                if index.files.len() >= options.max_files {
                    index.report.limit(ScanLimit::Files);
                    break 'walk;
                }
                if path.as_os_str().len() > options.max_total_path_bytes.saturating_sub(path_bytes)
                {
                    index.report.limit(ScanLimit::TotalPathBytes);
                    break 'walk;
                }
                path_bytes += path.as_os_str().len();
                index.files.push(path);
            } else {
                index.skipped += 1;
            }
        }
    }
    index.files.sort();
    index.truncated = index.report.cancelled
        || !index.report.limits.is_empty()
        || !index.report.errors.is_empty()
        || index.report.suppressed_errors != 0
        || index.report.non_utf8_skipped != 0;
    Ok(index)
}

fn stopped(
    report: &mut ScanReport,
    cancel: &AtomicBool,
    start: Instant,
    duration: Duration,
) -> bool {
    if cancel.load(Ordering::Relaxed) {
        report.cancelled = true;
        true
    } else if start.elapsed() >= duration {
        report.limit(ScanLimit::Time);
        true
    } else {
        false
    }
}

struct IgnoreRule {
    pattern: Vec<char>,
    anchored: bool,
    directory: bool,
}
impl IgnoreRule {
    fn parse(line: &str) -> Result<Option<Self>, ()> {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            return Ok(None);
        }
        if line.len() > MAX_PATTERN_BYTES
            || line.starts_with('!')
            || line.contains("**")
            || line.contains(['[', ']', '\\'])
        {
            return Err(());
        }
        let directory = line.ends_with('/');
        let anchored = line.starts_with('/') || line.trim_end_matches('/').contains('/');
        let pattern = line.trim_matches('/');
        if pattern.is_empty() {
            return Err(());
        }
        Ok(Some(Self {
            pattern: pattern.chars().collect(),
            anchored,
            directory,
        }))
    }
    fn matches(&self, path: &str, directory: bool) -> bool {
        if self.directory && !directory {
            return false;
        }
        let candidate = if self.anchored {
            path
        } else {
            path.rsplit('/').next().unwrap_or(path)
        };
        glob_matches(&self.pattern, candidate)
    }
}
fn ignore_rules(root: &Path, options: &ScanOptions, report: &mut ScanReport) -> Vec<IgnoreRule> {
    let mut rules = Vec::new();
    let mut count = 0;
    let mut add = |line: &str, source: &Path, report: &mut ScanReport| {
        if line.trim().is_empty() || line.trim().starts_with('#') {
            return;
        }
        count += 1;
        if count > MAX_IGNORE_RULES {
            report.limit(ScanLimit::IgnoreRules);
            return;
        }
        match IgnoreRule::parse(line) {
            Ok(Some(rule)) => rules.push(rule),
            Ok(None) => {}
            Err(()) => {
                report.unsupported_ignore_patterns += 1;
                report.error(
                    source,
                    format!(
                        "Unsupported ignore pattern: {}",
                        line.chars().take(80).collect::<String>()
                    ),
                );
            }
        }
    };
    for pattern in options.ignore_patterns.iter().take(MAX_IGNORE_RULES + 1) {
        add(pattern, root, report);
    }
    if options.ignore_patterns.len() > MAX_IGNORE_RULES {
        report.limit(ScanLimit::IgnoreRules);
    }
    if options.read_root_ignore_files {
        for name in [".gitignore", ".ignore"] {
            let path = root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(meta) if meta.is_file() && !meta.file_type().is_symlink() => {}
                Ok(_) => continue,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    report.error(&path, e.to_string());
                    continue;
                }
            }
            let mut bytes = Vec::new();
            let result = fs::File::open(&path).and_then(|file| {
                file.take((MAX_IGNORE_BYTES + 1) as u64)
                    .read_to_end(&mut bytes)
            });
            if let Err(error) = result {
                report.error(&path, error.to_string());
                continue;
            }
            if bytes.len() > MAX_IGNORE_BYTES {
                report.limit(ScanLimit::IgnoreFileBytes);
                continue;
            }
            match std::str::from_utf8(&bytes) {
                Ok(text) => {
                    for line in text.lines() {
                        add(line, &path, report);
                    }
                }
                Err(_) => report.error(&path, "Ignore file is not UTF-8"),
            }
        }
    }
    rules
}
fn glob_matches(pattern: &[char], text: &str) -> bool {
    let text: Vec<_> = text.chars().collect();
    let (mut p, mut t, mut star) = (0, 0, None);
    while t < text.len() {
        if p < pattern.len() && (pattern[p] == text[t] || (pattern[p] == '?' && text[t] != '/')) {
            p += 1;
            t += 1;
        } else if p < pattern.len() && pattern[p] == '*' {
            star = Some((p, t));
            p += 1;
        } else if let Some((s, matched)) = star {
            if matched >= text.len() || text[matched] == '/' {
                return false;
            }
            star = Some((s, matched + 1));
            p = s + 1;
            t = matched + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == '*' {
        p += 1;
    }
    p == pattern.len()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileMatch {
    pub file_index: usize,
    /// Slash-separated path relative to Index.root, suitable for display.
    pub relative_path: String,
    pub score: i64,
    /// UTF-8 byte ranges in `relative_path`, merged and sorted.
    pub matched_ranges: Vec<Range<usize>>,
}
struct Ranked(FileMatch);
impl PartialEq for Ranked {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other) == CmpOrdering::Equal
    }
}
impl Eq for Ranked {}
impl PartialOrd for Ranked {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}
impl Ord for Ranked {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        // Worst retained result on top; results remain stable for tied scores.
        other
            .0
            .score
            .cmp(&self.0.score)
            .then_with(|| self.0.relative_path.cmp(&other.0.relative_path))
            .then_with(|| self.0.file_index.cmp(&other.0.file_index))
    }
}

/// No filesystem access. At most MAX_RESULTS results; invalid/overlong queries
/// produce no results. Call validate_query to distinguish invalid from no match.
pub fn quick_open(index: &Index, query: &str, limit: usize) -> Vec<FileMatch> {
    quick_open_cancellable(index, query, limit, &AtomicBool::new(false))
}
/// Cancellation returns an empty list, never stale partial search results.
pub fn quick_open_cancellable(
    index: &Index,
    query: &str,
    limit: usize,
    cancel: &AtomicBool,
) -> Vec<FileMatch> {
    let limit = limit.min(MAX_RESULTS);
    if limit == 0 || validate_query(query).is_err() {
        return Vec::new();
    }
    let query = folded_query(query);
    let mut results = BinaryHeap::<Ranked>::new();
    for (file_index, path) in index.files.iter().enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Vec::new();
        }
        let Some(relative) = path.strip_prefix(&index.root).ok().and_then(Path::to_str) else {
            continue;
        };
        let relative_path = relative.replace(std::path::MAIN_SEPARATOR, "/");
        let Some((score, matched_ranges)) = score_path(&relative_path, &query) else {
            continue;
        };
        let candidate = Ranked(FileMatch {
            file_index,
            relative_path,
            score,
            matched_ranges,
        });
        if results.len() < limit {
            results.push(candidate);
        } else if results.peek().is_some_and(|worst| candidate < *worst) {
            results.pop();
            results.push(candidate);
        }
    }
    let mut results = results.into_sorted_vec();
    // into_sorted_vec is best-to-worst because Ranked reverses numeric scores.
    results.drain(..).map(|r| r.0).collect()
}
pub fn validate_query(query: &str) -> Result<(), NavigationError> {
    if query.chars().take(MAX_QUERY_CHARS + 1).count() > MAX_QUERY_CHARS {
        Err(NavigationError::QueryTooLong)
    } else {
        Ok(())
    }
}
fn folded_query(query: &str) -> Vec<char> {
    query
        .trim()
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect()
}
struct SearchChar {
    value: char,
    range: Range<usize>,
    boundary: bool,
}
fn search_chars(text: &str) -> Vec<SearchChar> {
    let mut chars = Vec::with_capacity(text.len());
    let mut previous = None;
    for (start, value) in text.char_indices() {
        let boundary = previous.is_none_or(|p: char| {
            !p.is_alphanumeric() || (p.is_lowercase() && value.is_uppercase())
        });
        for (i, folded) in value.to_lowercase().enumerate() {
            chars.push(SearchChar {
                value: folded,
                range: start..start + value.len_utf8(),
                boundary: boundary && i == 0,
            });
        }
        previous = Some(value);
    }
    chars
}
fn score_path(path: &str, query: &[char]) -> Option<(i64, Vec<Range<usize>>)> {
    let depth = path.chars().filter(|c| *c == '/').count() as i64;
    if query.is_empty() {
        return Some((-depth * 40 - path.chars().count() as i64, Vec::new()));
    }
    let chars = search_chars(path);
    let basename = path.rfind('/').map_or(0, |i| i + 1);
    let start = chars.partition_point(|c| c.range.start < basename);
    let mut best = fuzzy(&chars, query);
    if !query.contains(&'/') {
        if let Some((mut score, ranges)) = fuzzy(&chars[start..], query) {
            score += 10_000;
            let name = &chars[start..];
            if name.len() == query.len() && name.iter().map(|c| c.value).eq(query.iter().copied()) {
                score += 100_000;
            } else if name
                .iter()
                .take(query.len())
                .map(|c| c.value)
                .eq(query.iter().copied())
            {
                score += 50_000;
            }
            if best.as_ref().is_none_or(|(old, _)| score > *old) {
                best = Some((score, ranges));
            }
        }
    }
    best.map(|(score, ranges)| (score - depth * 40 - chars.len() as i64, ranges))
}
/// Forward subsequence matching plus reverse tightening. Inspect at most eight
/// windows: linear bounded work per candidate, no backtracking recursion or DP
/// allocation proportional to query length times path length.
fn fuzzy(chars: &[SearchChar], query: &[char]) -> Option<(i64, Vec<Range<usize>>)> {
    if query.is_empty() {
        return Some((0, Vec::new()));
    }
    let mut from = 0;
    let mut best = None;
    for _ in 0..8 {
        let (mut q, mut cursor) = (0, from);
        while cursor < chars.len() && q < query.len() {
            if chars[cursor].value == query[q] {
                q += 1;
            }
            cursor += 1;
        }
        if q != query.len() {
            break;
        }
        let mut positions = Vec::with_capacity(query.len());
        while q > 0 {
            cursor -= 1;
            if chars[cursor].value == query[q - 1] {
                positions.push(cursor);
                q -= 1;
            }
        }
        positions.reverse();
        from = positions[0] + 1;
        let mut score =
            1000 - (positions[positions.len() - 1] - positions[0]) as i64 * 4 - positions[0] as i64;
        let mut ranges: Vec<Range<usize>> = Vec::new();
        for (i, position) in positions.iter().copied().enumerate() {
            if chars[position].boundary {
                score += 48;
            }
            if i > 0 && position == positions[i - 1] + 1 {
                score += 64;
            }
            let range = chars[position].range.clone();
            if let Some(last) = ranges.last_mut().filter(|last| last.end >= range.start) {
                last.end = last.end.max(range.end);
            } else {
                ranges.push(range);
            }
        }
        if best.as_ref().is_none_or(|(old, _)| score > *old) {
            best = Some((score, ranges));
        }
    }
    best
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CommandDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub keywords: &'static str,
    pub shortcut: &'static str,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommandMatch {
    pub command_index: usize,
    pub score: i64,
}
/// Pure command filtering; callbacks and enabled state belong to the app.
pub fn filter_commands(
    commands: &[CommandDescriptor],
    query: &str,
    limit: usize,
) -> Vec<CommandMatch> {
    if validate_query(query).is_err() {
        return Vec::new();
    }
    let query = folded_query(query.trim().trim_start_matches('>'));
    let mut matches = commands
        .iter()
        .enumerate()
        .filter_map(|(command_index, command)| {
            let label = fuzzy(&search_chars(command.label), &query).map(|(s, _)| s + 500);
            let keywords = fuzzy(&search_chars(command.keywords), &query).map(|(s, _)| s);
            label
                .into_iter()
                .chain(keywords)
                .max()
                .map(|score| CommandMatch {
                    command_index,
                    score,
                })
        })
        .collect::<Vec<_>>();
    matches.sort_by(|a, b| {
        b.score
            .cmp(&a.score)
            .then(a.command_index.cmp(&b.command_index))
    });
    matches.truncate(limit.min(MAX_RESULTS));
    matches
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LineColumn {
    pub line: usize,
    pub column: usize,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NavigationError {
    InvalidLineColumn,
    QueryTooLong,
}
impl std::fmt::Display for NavigationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidLineColumn => {
                "Enter a positive line number, optionally followed by :column"
            }
            Self::QueryTooLong => "Navigation queries are limited to 128 characters",
        })
    }
}
impl std::error::Error for NavigationError {}
/// `42`, `42:7`, `:42:7`; ASCII decimal, one-based. No signs or zeroes.
pub fn parse_line_column(input: &str) -> Result<LineColumn, NavigationError> {
    let input = input.trim().strip_prefix(':').unwrap_or(input.trim());
    let mut parts = input.split(':');
    let parse = |part: &str| -> Result<usize, NavigationError> {
        let part = part.trim();
        if part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit()) {
            return Err(NavigationError::InvalidLineColumn);
        }
        part.parse::<usize>()
            .ok()
            .filter(|n| *n > 0)
            .ok_or(NavigationError::InvalidLineColumn)
    };
    let line = parse(parts.next().unwrap_or(""))?;
    let column = parts.next().map(parse).transpose()?.unwrap_or(1);
    if parts.next().is_some() {
        return Err(NavigationError::InvalidLineColumn);
    }
    Ok(LineColumn { line, column })
}
/// Columns count Unicode scalar values, not bytes/UTF-16 units or visual tab
/// cells. Lines are LF/CRLF; columns clamp before CR/LF, lines past EOF to EOF.
pub fn line_column_to_offset(text: &str, position: LineColumn) -> usize {
    let mut start = 0;
    for _ in 1..position.line.max(1) {
        let Some(newline) = text[start..].find('\n') else {
            return text.len();
        };
        start += newline + 1;
    }
    let end = text[start..].find('\n').map_or(text.len(), |n| start + n);
    let end = if end > start && text.as_bytes()[end - 1] == b'\r' {
        end - 1
    } else {
        end
    };
    text[start..end]
        .char_indices()
        .nth(position.column.saturating_sub(1))
        .map_or(end, |(byte, _)| start + byte)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Project(PathBuf);
    impl Project {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "kureha-navigation-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
        fn file(&self, name: &str, text: &str) {
            let path = self.0.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }
    impl Drop for Project {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn names(index: &Index) -> Vec<String> {
        index
            .files
            .iter()
            .map(|p| {
                p.strip_prefix(&index.root)
                    .unwrap()
                    .to_string_lossy()
                    .into_owned()
            })
            .collect()
    }
    #[test]
    fn scan_includes_hidden_files_but_prunes_build_vcs_and_hidden_directories() {
        let p = Project::new();
        for name in [
            "src/main.rs",
            "src/猫.rs",
            ".env",
            "target/debug/a",
            "node_modules/a.js",
            ".git/config",
            ".secret/file",
            "build/output",
        ] {
            p.file(name, "");
        }
        let index = scan(&p.0, &AtomicBool::new(false)).unwrap();
        assert_eq!(names(&index), [".env", "src/main.rs", "src/猫.rs"]);
        assert!(!index.truncated);
        assert_eq!(index.skipped, 5);
    }
    #[test]
    fn thousands_of_project_files_are_indexed_and_hidden_directories_are_opt_in() {
        let p = Project::new();
        for directory in 0..32 {
            for file in 0..64 {
                p.file(&format!("src/module{directory:02}/part{file:02}.rs"), "");
            }
        }
        p.file(".config/settings.json", "{}");
        p.file(".git/config", "");
        let options = ScanOptions {
            include_hidden_directories: true,
            ..ScanOptions::default()
        };
        let index = scan_with_options(&p.0, &options, &AtomicBool::new(false)).unwrap();
        assert!(!index.truncated, "{:?}", index.report);
        assert_eq!(index.files.len(), 2049);
        let hit = quick_open(&index, "module17/part42", 1).remove(0);
        assert_eq!(hit.relative_path, "src/module17/part42.rs");
        assert_eq!(
            fs::read_to_string(&index.files[hit.file_index]).unwrap(),
            ""
        );
        assert_eq!(
            quick_open(&index, "settings", 1)[0].relative_path,
            ".config/settings.json"
        );
        assert!(!index.files.iter().any(|path| path.ends_with(".git/config")));
    }
    #[test]
    fn root_ignore_subset_is_scoped_and_unsupported_syntax_is_visible() {
        let p = Project::new();
        p.file(
            ".gitignore",
            "*.log\n/cache/\nsrc/generated?.rs\n!keep.log\n**/nope\n",
        );
        for name in [
            "debug.log",
            "src/debug.log",
            "cache/a",
            "nested/cache/a",
            "src/generated1.rs",
            "src/main.rs",
        ] {
            p.file(name, "");
        }
        let index = scan(&p.0, &AtomicBool::new(false)).unwrap();
        assert_eq!(
            names(&index),
            [".gitignore", "nested/cache/a", "src/main.rs"]
        );
        assert_eq!(index.report.unsupported_ignore_patterns, 2);
        assert_eq!(index.report.errors.len(), 2);
        assert!(index.truncated);
    }
    #[test]
    fn scan_limits_and_cancellation_are_explicit() {
        let p = Project::new();
        for i in 0..10 {
            p.file(&format!("{i}.rs"), "");
        }
        let options = ScanOptions {
            max_files: 3,
            ..ScanOptions::default()
        };
        let index = scan_with_options(&p.0, &options, &AtomicBool::new(false)).unwrap();
        assert_eq!(index.files.len(), 3);
        assert!(index.truncated);
        assert_eq!(index.report.limits, [ScanLimit::Files]);
        let index = scan(&p.0, &AtomicBool::new(true)).unwrap();
        assert!(index.report.cancelled && index.truncated && index.files.is_empty());
        let options = ScanOptions {
            max_duration: Duration::ZERO,
            ..ScanOptions::default()
        };
        let index = scan_with_options(&p.0, &options, &AtomicBool::new(false)).unwrap();
        assert_eq!(index.report.limits, [ScanLimit::Time]);
    }
    #[test]
    fn depth_entries_and_storage_limits_preserve_readable_partial_results() {
        let p = Project::new();
        p.file("root.rs", "");
        p.file("a/deep.rs", "");
        for (options, expected) in [
            (
                ScanOptions {
                    max_depth: 0,
                    ..ScanOptions::default()
                },
                ScanLimit::Depth,
            ),
            (
                ScanOptions {
                    max_entries: 0,
                    ..ScanOptions::default()
                },
                ScanLimit::Entries,
            ),
            (
                ScanOptions {
                    max_directories: 0,
                    ..ScanOptions::default()
                },
                ScanLimit::Directories,
            ),
            (
                ScanOptions {
                    max_path_bytes: 1,
                    ..ScanOptions::default()
                },
                ScanLimit::PathBytes,
            ),
            (
                ScanOptions {
                    max_total_path_bytes: 0,
                    ..ScanOptions::default()
                },
                ScanLimit::TotalPathBytes,
            ),
        ] {
            let index = scan_with_options(&p.0, &options, &AtomicBool::new(false)).unwrap();
            assert!(index.truncated && index.report.limits.contains(&expected));
        }
    }
    #[cfg(unix)]
    #[test]
    fn scan_never_follows_file_directory_broken_or_root_symlinks() {
        use std::os::unix::fs::symlink;
        let p = Project::new();
        p.file("src/a", "");
        symlink(&p.0, p.0.join("loop")).unwrap();
        symlink(p.0.join("src/a"), p.0.join("link")).unwrap();
        symlink(p.0.join("missing"), p.0.join("broken")).unwrap();
        let index = scan(&p.0, &AtomicBool::new(false)).unwrap();
        assert_eq!(names(&index), ["src/a"]);
        assert_eq!(index.report.symlinks_skipped, 3);
        assert!(scan(&p.0.join("loop"), &AtomicBool::new(false)).is_err());
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn non_utf8_paths_are_reported_without_lossy_collisions() {
        use std::os::unix::ffi::OsStringExt;
        let p = Project::new();
        fs::write(p.0.join(std::ffi::OsString::from_vec(vec![255])), "").unwrap();
        let index = scan(&p.0, &AtomicBool::new(false)).unwrap();
        assert!(index.truncated && index.files.is_empty());
        assert_eq!(index.report.non_utf8_skipped, 1);
    }
    fn fixture(paths: &[&str]) -> Index {
        Index {
            root: "/project".into(),
            files: paths
                .iter()
                .map(|p| Path::new("/project").join(p))
                .collect(),
            ..Index::default()
        }
    }
    #[test]
    fn fuzzy_ranks_basename_prefix_above_directory_hits_and_keeps_stable_ties() {
        let index = fixture(&[
            "main.rs/sub.rs",
            "src/main.rs",
            "src/my_array_impl_new.rs",
            "main.rs",
        ]);
        let hits = quick_open(&index, "main", 20);
        assert_eq!(hits[0].relative_path, "main.rs");
        assert_eq!(hits[1].relative_path, "src/main.rs");
        assert_eq!(hits.last().unwrap().relative_path, "main.rs/sub.rs");
        assert_eq!(
            quick_open(&index, "src/main", 20)[0].relative_path,
            "src/main.rs"
        );
        assert!(quick_open(&index, "zzz", 20).is_empty());
    }
    #[test]
    fn unicode_case_folding_ranges_always_slice_the_original_display_path() {
        let index = fixture(&["資料/İ猫🍁.rs", "資料/猫町.rs", "unicode/École.rs"]);
        for query in ["i猫", "猫🍁", "éco", "資料/猫"] {
            let hits = quick_open(&index, query, 20);
            assert!(!hits.is_empty(), "{query}");
            for hit in hits {
                for range in hit.matched_ranges {
                    assert!(hit.relative_path.get(range).is_some());
                }
            }
        }
        assert_eq!(
            quick_open(&index, "猫🍁", 1)[0].relative_path,
            "資料/İ猫🍁.rs"
        );
    }
    #[test]
    fn large_indexes_are_bounded_and_cancellable_without_filesystem_reads() {
        let index = Index {
            root: "/project".into(),
            files: (0..12_000)
                .map(|i| format!("/project/src/file{i:05}.rs").into())
                .collect(),
            ..Index::default()
        };
        assert_eq!(quick_open(&index, "", usize::MAX).len(), MAX_RESULTS);
        assert_eq!(
            quick_open(&index, "file11999", 10)[0].relative_path,
            "src/file11999.rs"
        );
        assert!(quick_open_cancellable(&index, "file", 10, &AtomicBool::new(true)).is_empty());
        assert!(validate_query(&"猫".repeat(MAX_QUERY_CHARS + 1)).is_err());
        assert!(quick_open(&index, "file", 0).is_empty());
    }
    #[test]
    fn command_filter_is_pure_and_supports_keywords_and_palette_prefix() {
        let commands = [
            CommandDescriptor {
                id: "folder",
                label: "File: Open Folder",
                keywords: "workspace project",
                shortcut: "Cmd+Shift+O",
            },
            CommandDescriptor {
                id: "line",
                label: "Go to Line",
                keywords: "navigate row column",
                shortcut: "Ctrl+G",
            },
        ];
        assert_eq!(
            filter_commands(&commands, ">opfold", 10)[0].command_index,
            0
        );
        assert_eq!(filter_commands(&commands, "row", 10)[0].command_index, 1);
        assert_eq!(filter_commands(&commands, "", 1).len(), 1);
    }
    #[test]
    fn line_column_parser_rejects_ambiguous_input_and_overflow() {
        assert_eq!(
            parse_line_column(" :42:7 "),
            Ok(LineColumn {
                line: 42,
                column: 7
            })
        );
        assert_eq!(
            parse_line_column("8"),
            Ok(LineColumn { line: 8, column: 1 })
        );
        for bad in [
            "",
            "0",
            "2:0",
            "-1",
            "+3",
            "1:",
            "1:2:3",
            "99999999999999999999999999999999",
            "１",
            "1.5",
        ] {
            assert!(parse_line_column(bad).is_err(), "{bad}");
        }
    }
    #[test]
    fn goto_clamps_columns_and_lines_with_unicode_crlf_empty_and_trailing_lines() {
        let text = "猫🍁x\r\nsecond\n";
        assert_eq!(
            line_column_to_offset(text, LineColumn { line: 1, column: 2 }),
            3
        );
        assert_eq!(
            line_column_to_offset(text, LineColumn { line: 1, column: 3 }),
            7
        );
        assert_eq!(
            line_column_to_offset(
                text,
                LineColumn {
                    line: 1,
                    column: 100
                }
            ),
            8
        );
        assert_eq!(
            line_column_to_offset(text, LineColumn { line: 2, column: 1 }),
            10
        );
        assert_eq!(
            line_column_to_offset(text, LineColumn { line: 3, column: 1 }),
            text.len()
        );
        assert_eq!(
            line_column_to_offset(
                text,
                LineColumn {
                    line: usize::MAX,
                    column: 1
                }
            ),
            text.len()
        );
        assert_eq!(
            line_column_to_offset("", LineColumn { line: 1, column: 1 }),
            0
        );
    }
}
