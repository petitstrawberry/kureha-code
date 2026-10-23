//! Workspace search runs on a worker; results keep the exact source snapshot
//! used by replacement previews. No search or replacement writes to disk.
use crate::{
    files,
    search::{Options, Pattern},
};
use std::{
    collections::BTreeMap,
    ops::Range,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};

pub const MAX_MATCHES: usize = 2_000;
pub const MAX_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;
pub const MAX_REPLACE_FILES: usize = 128;

#[derive(Clone)]
pub struct Source {
    pub path: PathBuf,
    pub text: Arc<str>,
    /// The raw disk contents remain distinct from normalized editor contents.
    pub disk_text: Option<Arc<str>>,
    pub document: Option<(u64, u64)>,
}
#[derive(Clone, Debug)]
pub struct Hit {
    pub source: usize,
    pub range: Range<usize>,
    pub line: usize,
    pub column: usize,
    pub preview: String,
}
pub struct Results {
    pub sources: Vec<Source>,
    pub hits: Vec<Hit>,
    pub skipped: usize,
    pub truncated: bool,
    pub cancelled: bool,
    pub query: String,
    pub options: Options,
}
impl Results {
    pub fn summary(&self) -> String {
        format!(
            "{} matches in {} files{}{}",
            self.hits.len(),
            self.sources.len(),
            if self.truncated {
                " · result limit reached"
            } else if self.cancelled {
                " · cancelled"
            } else {
                ""
            },
            if self.skipped > 0 {
                format!(
                    " · {} binary, oversized or unreadable files skipped",
                    self.skipped
                )
            } else {
                String::new()
            }
        )
    }
}
pub fn scan(
    paths: &[PathBuf],
    open: BTreeMap<PathBuf, Source>,
    query: String,
    options: Options,
    cancel: &AtomicBool,
) -> Result<Results, String> {
    let pattern = Pattern::new(&query, options)?;
    let mut result = Results {
        sources: Vec::new(),
        hits: Vec::new(),
        skipped: 0,
        truncated: false,
        cancelled: false,
        query,
        options,
    };
    let mut paths: Vec<_> = paths.iter().cloned().chain(open.keys().cloned()).collect();
    paths.sort();
    paths.dedup();
    let started = Instant::now();
    let mut bytes = 0usize;
    for path in paths {
        if cancel.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }
        if started.elapsed() > Duration::from_secs(15) {
            result.truncated = true;
            break;
        }
        let source = if let Some(source) = open.get(&path) {
            source.clone()
        } else {
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(m) if m.is_file() && m.len() <= MAX_FILE_BYTES as u64 => m,
                _ => {
                    result.skipped += 1;
                    continue;
                }
            };
            let _ = metadata;
            let raw = match files::read(&path) {
                Ok(text) if text.len() <= MAX_FILE_BYTES && !text.contains('\0') => text,
                _ => {
                    result.skipped += 1;
                    continue;
                }
            };
            let text: Arc<str> = raw.replace("\r\n", "\n").replace('\r', "\n").into();
            Source {
                path: path.clone(),
                text,
                disk_text: Some(raw.into()),
                document: None,
            }
        };
        if source.text.len() > MAX_FILE_BYTES {
            result.skipped += 1;
            continue;
        }
        bytes = bytes.saturating_add(source.text.len());
        if bytes > MAX_TOTAL_BYTES {
            result.truncated = true;
            break;
        }
        let source_index = result.sources.len();
        let mut line = 1;
        let mut cursor = 0;
        let mut line_start = 0;
        for range in pattern.ranges(&source.text) {
            if result.hits.len() == MAX_MATCHES {
                result.truncated = true;
                break;
            }
            for (offset, _) in source.text[cursor..range.start].match_indices('\n') {
                line += 1;
                line_start = cursor + offset + 1;
            }
            let end = source.text[range.start..]
                .find('\n')
                .map_or(source.text.len(), |p| range.start + p);
            let column = source.text[line_start..range.start].chars().count() + 1;
            let preview = source.text[line_start..end].chars().take(180).collect();
            result.hits.push(Hit {
                source: source_index,
                range,
                line,
                column,
                preview,
            });
            // The next iteration counts only new line endings, even with
            // multiple matches on the same line or a multiline expression.
            cursor = result.hits.last().unwrap().range.start;
        }
        if result.hits.last().is_some_and(|h| h.source == source_index) {
            result.sources.push(source);
        }
        if result.truncated {
            break;
        }
    }
    Ok(result)
}

pub struct Replacement {
    pub parts: Vec<scarlet_ui::prelude::EditDelta>,
    pub source: Source,
    pub text: String,
    pub matches: usize,
}
pub fn prepare_replace(
    results: &Results,
    replacement: &str,
    cancel: &AtomicBool,
) -> Result<Vec<Replacement>, String> {
    if results.truncated || results.cancelled {
        return Err("Run a complete search before replacing its results".into());
    }
    if results.sources.len() > MAX_REPLACE_FILES {
        return Err("Narrow the search to at most 128 files before replacing".into());
    }
    let pattern = Pattern::new(&results.query, results.options)?;
    let mut changes = Vec::new();
    for source in &results.sources {
        if cancel.load(Ordering::Relaxed) {
            return Err("Replacement cancelled; documents unchanged".into());
        }
        if source.document.is_none()
            && files::read(&source.path).ok().as_deref() != source.disk_text.as_deref()
        {
            return Err(format!(
                "{} changed on disk; search again before replacing",
                source.path.display()
            ));
        }
        if let Some((text, parts)) = pattern.replacement_plan(
            &source.text,
            replacement,
            results.options.regex,
            files::MAX_BYTES as usize,
        )? {
            changes.push(Replacement {
                matches: parts.len(),
                parts,
                source: source.clone(),
                text,
            });
        }
    }
    Ok(changes)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unicode_and_multiline_matches_use_open_unsaved_content() {
        let path = PathBuf::from("/project/source.rs");
        let open = BTreeMap::from([(
            path.clone(),
            Source {
                path: path.clone(),
                text: "猫 cats\n猫 cats".into(),
                disk_text: None,
                document: Some((7, 3)),
            },
        )]);
        let result = scan(
            &[],
            open,
            "猫|cats".into(),
            Options {
                regex: true,
                ..Options::default()
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            result
                .hits
                .iter()
                .map(|h| (h.line, h.column))
                .collect::<Vec<_>>(),
            [(1, 1), (1, 3), (2, 1), (2, 3)]
        );
        let changes = prepare_replace(&result, "x", &AtomicBool::new(false)).unwrap();
        assert_eq!(changes[0].text, "x x\nx x");
        assert_eq!(changes[0].source.document, Some((7, 3)));
    }
}
