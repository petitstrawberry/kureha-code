//! The lines of one change, like `git diff` with three lines of context:
//! `HEAD` to the index for a staged change, the index to the work tree for
//! an unstaged one. The work tree side is converted to its Git form first
//! (line endings, for example), as staging it would, so only real changes
//! show. Filters from the repository's own config never run (see the module
//! above).

use super::imp;
use gix::diff::blob::{Algorithm, Diff, InternedInput};
use std::path::Path;

/// Bytes read from either side; larger files are not compared.
pub const MAX_BYTES: usize = 8 * 1024 * 1024;
/// Lines kept for one change.
pub const MAX_LINES: usize = 100_000;
/// Unchanged lines shown around each change.
const CONTEXT: u32 = 3;
/// Bytes checked for a NUL, as Git does, to tell binary files.
const BINARY_PROBE: usize = 8000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Side {
    /// `HEAD` to the index: what a commit would record.
    Staged,
    /// The index to the work tree: what staging would add.
    Unstaged,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    /// `@@ -1,3 +1,4 @@`
    Hunk,
    Context,
    Removed,
    Added,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Line {
    pub kind: Kind,
    /// 1-based line numbers on each side, where the line exists.
    pub old: Option<u32>,
    pub new: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FileDiff {
    pub lines: Vec<Line>,
    pub added: usize,
    pub removed: usize,
    /// Why some or all lines are not shown (binary, too large).
    pub note: Option<String>,
}

/// Compares two versions line by line.
pub fn lines(before: &[u8], after: &[u8]) -> FileDiff {
    if [before, after]
        .iter()
        .any(|side| side[..side.len().min(BINARY_PROBE)].contains(&0))
    {
        return FileDiff {
            note: Some("Binary file".into()),
            ..FileDiff::default()
        };
    }
    if before.len() > MAX_BYTES || after.len() > MAX_BYTES {
        return FileDiff {
            note: Some(format!(
                "Larger than {} MiB; not compared",
                MAX_BYTES / (1024 * 1024)
            )),
            ..FileDiff::default()
        };
    }
    let input = InternedInput::new(before, after);
    let mut diff = Diff::compute(Algorithm::Histogram, &input);
    diff.postprocess_lines(&input);
    let text = |token: gix::diff::blob::Token| {
        let bytes: &[u8] = input.interner[token];
        let bytes = bytes.strip_suffix(b"\n").unwrap_or(bytes);
        let bytes = bytes.strip_suffix(b"\r").unwrap_or(bytes);
        String::from_utf8_lossy(bytes).into_owned()
    };
    let old_len = input.before.len() as u32;
    // Hunks closer than twice the context share one header, as in Git.
    let mut groups: Vec<Vec<gix::diff::blob::Hunk>> = Vec::new();
    for hunk in diff.hunks() {
        match groups.last_mut() {
            Some(group) if hunk.before.start - group.last().unwrap().before.end <= 2 * CONTEXT => {
                group.push(hunk)
            }
            _ => groups.push(vec![hunk]),
        }
    }
    let mut out = FileDiff::default();
    for group in groups {
        let (first, last) = (&group[0], &group[group.len() - 1]);
        let lead = first.before.start.min(CONTEXT);
        let (mut old, mut new) = (first.before.start - lead, first.after.start - lead);
        let old_end = (last.before.end + CONTEXT).min(old_len);
        let new_end = old_end - last.before.end + last.after.end;
        // Git numbers an empty range by the line before it.
        let start = |from: u32, to: u32| if to > from { from + 1 } else { from };
        out.lines.push(Line {
            kind: Kind::Hunk,
            old: None,
            new: None,
            text: format!(
                "@@ -{},{} +{},{} @@",
                start(old, old_end),
                old_end - old,
                start(new, new_end),
                new_end - new
            ),
        });
        let context = |old: &mut u32, new: &mut u32, until: u32, out: &mut FileDiff| {
            while *old < until {
                out.lines.push(Line {
                    kind: Kind::Context,
                    old: Some(*old + 1),
                    new: Some(*new + 1),
                    text: text(input.before[*old as usize]),
                });
                *old += 1;
                *new += 1;
            }
        };
        for hunk in &group {
            context(&mut old, &mut new, hunk.before.start, &mut out);
            for line in hunk.before.clone() {
                out.lines.push(Line {
                    kind: Kind::Removed,
                    old: Some(line + 1),
                    new: None,
                    text: text(input.before[line as usize]),
                });
            }
            for line in hunk.after.clone() {
                out.lines.push(Line {
                    kind: Kind::Added,
                    old: None,
                    new: Some(line + 1),
                    text: text(input.after[line as usize]),
                });
            }
            out.removed += hunk.before.len();
            out.added += hunk.after.len();
            (old, new) = (hunk.before.end, hunk.after.end);
        }
        context(&mut old, &mut new, old_end, &mut out);
    }
    if out.lines.len() > MAX_LINES {
        out.lines.truncate(MAX_LINES);
        out.note = Some(format!("Showing the first {MAX_LINES} lines"));
    }
    out
}

/// Reads the change of `path` (relative to the work tree) in the repository
/// containing `folder`.
pub fn read(folder: &Path, path: &Path, side: Side) -> Result<FileDiff, String> {
    let repo = imp::open(folder)?.ok_or("Not a Git repository")?;
    let workdir = repo
        .workdir()
        .ok_or("The repository has no work tree")?
        .to_path_buf();
    let rela = path.to_str().ok_or("The path is not valid UTF-8")?;
    let index = repo.index_or_empty().map_err(|e| e.to_string())?;
    let stage = |stage| {
        index
            .entry_by_path_and_stage(rela.into(), stage)
            .map(|entry| (entry.id, entry.mode))
    };
    let blob =
        |found: Option<(gix::ObjectId, gix::index::entry::Mode)>| -> Result<Vec<u8>, String> {
            match found {
                None => Ok(Vec::new()),
                Some((id, mode)) if mode == gix::index::entry::Mode::COMMIT => {
                    Ok(format!("Subproject commit {id}\n").into_bytes())
                }
                Some((id, _)) => {
                    let object = repo.find_object(id).map_err(|e| e.to_string())?;
                    Ok(object.data.clone())
                }
            }
        };
    use gix::index::entry::Stage;
    let (before, after) = match side {
        Side::Staged => {
            let tree = repo.head_tree_id_or_empty().map_err(|e| e.to_string())?;
            let tree = repo.find_tree(tree).map_err(|e| e.to_string())?;
            let head = tree
                .lookup_entry_by_path(path)
                .map_err(|e| e.to_string())?
                .map(|entry| {
                    let mode = if entry.mode().is_commit() {
                        gix::index::entry::Mode::COMMIT
                    } else {
                        gix::index::entry::Mode::FILE
                    };
                    (entry.object_id(), mode)
                });
            (blob(head)?, blob(stage(Stage::Unconflicted))?)
        }
        Side::Unstaged => {
            // A conflicted path compares the work tree with our side.
            let base = stage(Stage::Unconflicted).or_else(|| stage(Stage::Ours));
            let after = worktree(&repo, &workdir, path, &index)?;
            (blob(base)?, after)
        }
    };
    Ok(lines(&before, &after))
}

/// The work tree's version of `path` in its Git form, empty if it is gone.
fn worktree(
    repo: &gix::Repository,
    workdir: &Path,
    path: &Path,
    index: &gix::index::State,
) -> Result<Vec<u8>, String> {
    let full = workdir.join(path);
    let metadata = match std::fs::symlink_metadata(&full) {
        Ok(metadata) => metadata,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    if metadata.is_symlink() {
        let target = std::fs::read_link(&full).map_err(|e| e.to_string())?;
        return Ok(gix::path::into_bstr(target)
            .map_err(|e| e.to_string())?
            .to_vec());
    }
    if metadata.is_dir() {
        return Ok(Vec::new());
    }
    if metadata.len() > MAX_BYTES as u64 {
        // Report it as too large without reading it.
        return Ok(vec![b'\n'; MAX_BYTES + 1]);
    }
    let file = std::fs::File::open(&full).map_err(|e| e.to_string())?;
    let (mut pipeline, _) = repo.filter_pipeline(None).map_err(|e| e.to_string())?;
    read_converted(&mut pipeline, file, path, index)
}

/// `src` converted to its Git form for `path`.
pub(super) fn read_converted(
    pipeline: &mut gix::filter::Pipeline<'_>,
    src: impl std::io::Read,
    path: &Path,
    index: &gix::index::State,
) -> Result<Vec<u8>, String> {
    use gix::filter::plumbing::pipeline::convert::ToGitOutcome;
    use std::io::Read;
    let mut out = Vec::new();
    match pipeline
        .convert_to_git(src, path, index)
        .map_err(|e| e.to_string())?
    {
        ToGitOutcome::Unchanged(mut reader) => reader.read_to_end(&mut out),
        ToGitOutcome::Process(mut reader) => reader.read_to_end(&mut out),
        ToGitOutcome::Buffer(buffer) => {
            out.extend_from_slice(buffer);
            Ok(buffer.len())
        }
    }
    .map_err(|e| e.to_string())?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::testing::Repo;

    fn render(diff: &FileDiff) -> String {
        diff.lines
            .iter()
            .map(|line| {
                let mark = match line.kind {
                    Kind::Hunk => "",
                    Kind::Context => " ",
                    Kind::Removed => "-",
                    Kind::Added => "+",
                };
                format!("{mark}{}\n", line.text)
            })
            .collect()
    }

    #[test]
    fn hunks_carry_three_lines_of_context_and_merge_when_close() {
        let before: String = (1..=20).map(|i| format!("line {i}\n")).collect();
        let after = before
            .replace("line 2\n", "line two\n")
            .replace("line 8\n", "line eight\n")
            .replace("line 18\n", "")
            + "line 21\n";
        let diff = lines(before.as_bytes(), after.as_bytes());
        assert_eq!(
            render(&diff),
            "@@ -1,11 +1,11 @@\n line 1\n-line 2\n+line two\n line 3\n line 4\n line 5\n line 6\n line 7\n-line 8\n+line eight\n line 9\n line 10\n line 11\n\
             @@ -15,6 +15,6 @@\n line 15\n line 16\n line 17\n-line 18\n line 19\n line 20\n+line 21\n"
        );
        assert_eq!((diff.added, diff.removed), (3, 3));
        let added = diff.lines.iter().find(|l| l.kind == Kind::Added).unwrap();
        assert_eq!((added.old, added.new), (None, Some(2)));
        let removed = diff.lines.iter().find(|l| l.text == "line 18").unwrap();
        assert_eq!((removed.old, removed.new), (Some(18), None));
    }

    #[test]
    fn new_and_deleted_files_and_binaries() {
        let added = lines(b"", b"a\nb\n");
        assert_eq!(render(&added), "@@ -0,0 +1,2 @@\n+a\n+b\n");
        let deleted = lines(b"a\r\n", b"");
        assert_eq!(render(&deleted), "@@ -1,1 +0,0 @@\n-a\n");
        assert_eq!(lines(b"same\n", b"same\n"), FileDiff::default());
        assert_eq!(lines(b"a\0b", b"").note.as_deref(), Some("Binary file"));
    }

    #[test]
    fn reads_both_sides_in_git_form() {
        let repo = Repo::new("diff");
        repo.write(".gitattributes", "*.txt text eol=lf\n");
        repo.write("a.txt", "one\ntwo\nthree\n");
        repo.git(&["add", "."]);
        repo.git(&["commit", "-q", "-m", "init"]);
        // Line endings alone are not a change once converted.
        repo.write("a.txt", "one\r\n2\r\nthree\r\n");
        let expected = "@@ -1,3 +1,3 @@\n one\n-two\n+2\n three\n";
        let a = Path::new("a.txt");
        assert_eq!(render(&read(&repo.0, a, Side::Unstaged).unwrap()), expected);
        assert_eq!(read(&repo.0, a, Side::Staged).unwrap(), FileDiff::default());
        repo.git(&["add", "a.txt"]);
        assert_eq!(render(&read(&repo.0, a, Side::Staged).unwrap()), expected);
        assert_eq!(
            read(&repo.0, a, Side::Unstaged).unwrap(),
            FileDiff::default()
        );
        repo.write("new.txt", "n\n");
        assert_eq!(
            render(&read(&repo.0, Path::new("new.txt"), Side::Unstaged).unwrap()),
            "@@ -0,0 +1,1 @@\n+n\n"
        );
        std::fs::remove_file(repo.0.join("a.txt")).unwrap();
        assert_eq!(
            render(&read(&repo.0, a, Side::Unstaged).unwrap()),
            "@@ -1,3 +0,0 @@\n-one\n-2\n-three\n"
        );
    }
}
